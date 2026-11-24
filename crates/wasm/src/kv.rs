//! 외부 모듈의 키-값 저장소 (`ext_module_kv`, ADR-0177). 모듈은 자기 행만 본다.
//!
//! 모듈이 내부에 쓰는 값이라 감사 기록에는 남기지 않는다(내장 모듈의 내부 표와 같다).
//! 사람에게 보이는 쓰기(글·댓글 등)는 3단계 호스트 API 가 감사에 남긴다.

use giwa_core::clock;
use giwa_storage::Db;

pub const MAX_KEY: usize = 200;
pub const MAX_VALUE: usize = 64 * 1024;
/// 모듈마다 키와 값을 합친 바이트
pub const MAX_TOTAL: i64 = 50 * 1000 * 1000;
pub const MAX_LIST: u32 = 100;

/// 손님에게 돌려줄 실패. (코드, 문장)
pub type KvError = (&'static str, String);

fn internal(e: sqlx::Error) -> KvError {
    tracing::warn!(error = ?e, "외부 모듈 키-값 저장소 오류");
    ("internal", "저장소를 읽거나 쓰지 못했습니다.".to_owned())
}

fn check_key(key: &str) -> Result<(), KvError> {
    if key.is_empty() || key.len() > MAX_KEY {
        return Err(("validation", format!("키는 1~{MAX_KEY}바이트입니다.")));
    }
    Ok(())
}

pub async fn get(db: &Db, module: &str, key: &str) -> Result<Option<Vec<u8>>, KvError> {
    check_key(key)?;
    sqlx::query_scalar!(
        "SELECT value FROM ext_module_kv WHERE module = ? AND key = ?",
        module,
        key
    )
    .fetch_optional(db.read_pool())
    .await
    .map_err(internal)
}

pub async fn set(db: &Db, module: &str, key: &str, value: &[u8]) -> Result<(), KvError> {
    check_key(key)?;
    if value.len() > MAX_VALUE {
        return Err((
            "validation",
            format!("값은 {}KB 까지입니다.", MAX_VALUE / 1024),
        ));
    }
    let mut tx = db
        .write_pool()
        .begin_with("BEGIN IMMEDIATE")
        .await
        .map_err(internal)?;
    let others = sqlx::query_scalar!(
        r#"SELECT COALESCE(SUM(LENGTH(key) + LENGTH(value)), 0) AS "n!: i64"
           FROM ext_module_kv WHERE module = ? AND key <> ?"#,
        module,
        key
    )
    .fetch_one(&mut *tx)
    .await
    .map_err(internal)?;
    let size = i64::try_from(key.len() + value.len()).unwrap_or(i64::MAX);
    if others.saturating_add(size) > MAX_TOTAL {
        return Err((
            "conflict",
            format!(
                "이 모듈의 저장소가 {}MB 를 넘습니다. 오래된 값을 지우세요.",
                MAX_TOTAL / 1_000_000
            ),
        ));
    }
    let now = clock::now();
    sqlx::query!(
        "INSERT INTO ext_module_kv (module, key, value, updated_at) VALUES (?, ?, ?, ?)
         ON CONFLICT (module, key) DO UPDATE SET value = excluded.value, updated_at = excluded.updated_at",
        module,
        key,
        value,
        now
    )
    .execute(&mut *tx)
    .await
    .map_err(internal)?;
    tx.commit().await.map_err(internal)
}

pub async fn delete(db: &Db, module: &str, key: &str) -> Result<bool, KvError> {
    check_key(key)?;
    let done = sqlx::query!(
        "DELETE FROM ext_module_kv WHERE module = ? AND key = ?",
        module,
        key
    )
    .execute(db.write_pool())
    .await
    .map_err(internal)?;
    Ok(done.rows_affected() > 0)
}

/// `prefix` 로 시작하는 키를 키 순서대로, `after` 다음부터.
pub async fn list(
    db: &Db,
    module: &str,
    prefix: &str,
    after: Option<&str>,
    limit: u32,
) -> Result<Vec<(String, Vec<u8>)>, KvError> {
    if prefix.len() > MAX_KEY {
        return Err(("validation", format!("앞부분은 {MAX_KEY}바이트까지입니다.")));
    }
    let limit = i64::from(limit.clamp(1, MAX_LIST));
    let after = after.unwrap_or("");
    let rows = sqlx::query!(
        r#"SELECT key AS "key!", value AS "value!" FROM ext_module_kv
           WHERE module = ? AND substr(key, 1, length(?)) = ? AND key > ?
           ORDER BY key LIMIT ?"#,
        module,
        prefix,
        prefix,
        after,
        limit
    )
    .fetch_all(db.read_pool())
    .await
    .map_err(internal)?;
    Ok(rows.into_iter().map(|r| (r.key, r.value)).collect())
}

/// 모듈을 제거할 때 그 모듈의 행만 지운다. 모듈 id 는 매니페스트 검사를 거친 `[a-z0-9_]` 다.
pub fn cleanup_sql(module: &str) -> String {
    format!("DELETE FROM ext_module_kv WHERE module = '{module}';")
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::*;

    async fn db() -> (tempfile::TempDir, Db) {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::open(&dir.path().join("t.db"), true).await.unwrap();
        db.migrate().await.unwrap();
        (dir, db)
    }

    #[tokio::test]
    async fn keeps_modules_apart_and_lists_by_prefix() {
        let (_dir, db) = db().await;
        set(&db, "a", "n:1", b"one").await.unwrap();
        set(&db, "a", "n:2", b"two").await.unwrap();
        set(&db, "a", "x", b"other").await.unwrap();
        set(&db, "b", "n:1", b"b").await.unwrap();
        set(&db, "a", "n:1", b"uno").await.unwrap();
        assert_eq!(get(&db, "a", "n:1").await.unwrap().unwrap(), b"uno");
        assert_eq!(get(&db, "b", "n:1").await.unwrap().unwrap(), b"b");
        let keys: Vec<String> = list(&db, "a", "n:", None, 10)
            .await
            .unwrap()
            .into_iter()
            .map(|(k, _)| k)
            .collect();
        assert_eq!(keys, ["n:1", "n:2"]);
        let after = list(&db, "a", "n:", Some("n:1"), 10).await.unwrap();
        assert_eq!(after.len(), 1);
        assert!(delete(&db, "a", "n:1").await.unwrap());
        assert!(!delete(&db, "a", "n:1").await.unwrap());
        sqlx::raw_sql(sqlx::AssertSqlSafe(cleanup_sql("a")))
            .execute(db.write_pool())
            .await
            .unwrap();
        assert!(get(&db, "a", "x").await.unwrap().is_none());
        assert!(get(&db, "b", "n:1").await.unwrap().is_some());
    }

    #[tokio::test]
    async fn rejects_oversized_keys_and_values() {
        let (_dir, db) = db().await;
        assert!(set(&db, "a", "", b"v").await.is_err());
        assert!(set(&db, "a", &"k".repeat(MAX_KEY + 1), b"v").await.is_err());
        assert!(set(&db, "a", "k", &vec![0; MAX_VALUE + 1]).await.is_err());
        assert!(set(&db, "a", "k", &vec![0; MAX_VALUE]).await.is_ok());
    }
}
