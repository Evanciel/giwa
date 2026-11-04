//! 이벤트 배달 (ADR-0066). 켜진 모듈마다 `audit_log` 의 커서를 따라가며 구독한 동작을 넘긴다.
//!
//! 최소 한 번 배달이다. 실패한 이벤트는 실패 표에 적고 커서는 넘어간다(글 하나 때문에 모듈 전체가
//! 멈추지 않게). 정한 횟수만큼 다시 시도하고, 그래도 안 되면 실패 보관함에 남긴다.

use maru_core::clock;
use maru_core::model::{AuditEvent, ModuleFailure};
use maru_core::{Ctx, Result};

use crate::{Module, Registry};

/// 한 번에 읽는 이벤트 수.
const BATCH: i64 = 200;
/// 이만큼 실패하면 다시 시도하지 않는다.
pub const MAX_ATTEMPTS: i64 = 5;
/// 재시도 간격(분). `attempts` 번째 실패 뒤에 쓴다.
const BACKOFF_MINUTES: [i64; 4] = [1, 5, 30, 120];
/// 오류 문장을 이 글자 수에서 자른다.
const MAX_ERROR: usize = 500;

/// 밀린 이벤트를 모두 넘긴다. 돌려주는 값은 처음 넘긴 이벤트 수(재시도 제외).
pub async fn deliver_once(ctx: &Ctx, registry: &Registry) -> Result<usize> {
    let mut delivered = 0;
    let rows = ctx.store.module_rows().await?;
    for row in rows.iter().filter(|r| r.enabled) {
        let Some(module) = registry.get(&row.id) else {
            continue;
        };
        let events = module.manifest().events;
        if events.is_empty() {
            continue;
        }
        retry_due(ctx, module.as_ref()).await?;
        // 끝을 먼저 읽어 그 사이 새로 적힌 기록을 건너뛰지 않는다.
        let upto = ctx.store.audit_last_seq().await?;
        if upto <= row.cursor {
            continue;
        }
        let mut after = row.cursor;
        loop {
            let batch = ctx.store.audit_after(after, upto, events, BATCH).await?;
            for ev in &batch {
                delivered += 1;
                if let Err(e) = module.on_event(ctx, ev).await {
                    tracing::warn!(module = %row.id, event = %ev.id, action = %ev.action, error = ?e, "모듈 이벤트 처리 실패");
                    let failure = ModuleFailure {
                        module: row.id.clone(),
                        event_id: ev.id.clone(),
                        action: ev.action.clone(),
                        attempts: 1,
                        last_error: short(&e.message()),
                        next_at: clock::minutes_later(BACKOFF_MINUTES[0]),
                        gave_up: false,
                        first_at: clock::now(),
                    };
                    ctx.store.module_failure_put(&failure).await?;
                }
            }
            match batch.last() {
                Some(last) if batch.len() as i64 == BATCH => after = last.seq,
                _ => break,
            }
        }
        ctx.store.module_cursor_set(&row.id, upto).await?;
    }
    Ok(delivered)
}

async fn retry_due(ctx: &Ctx, module: &dyn Module) -> Result<()> {
    let id = module.manifest().id;
    let due = ctx.store.module_failures_due(id, &clock::now()).await?;
    for mut f in due {
        let Some(ev): Option<AuditEvent> = ctx.store.audit_event(&f.event_id).await? else {
            ctx.store.module_failure_delete(id, &f.event_id).await?;
            continue;
        };
        match module.on_event(ctx, &ev).await {
            Ok(()) => ctx.store.module_failure_delete(id, &f.event_id).await?,
            Err(e) => {
                f.attempts += 1;
                f.last_error = short(&e.message());
                f.gave_up = f.attempts >= MAX_ATTEMPTS;
                let wait = BACKOFF_MINUTES
                    .get(f.attempts as usize - 1)
                    .copied()
                    .unwrap_or(BACKOFF_MINUTES[BACKOFF_MINUTES.len() - 1]);
                f.next_at = clock::minutes_later(wait);
                ctx.store.module_failure_put(&f).await?;
            }
        }
    }
    Ok(())
}

fn short(text: &str) -> String {
    text.chars().take(MAX_ERROR).collect()
}
