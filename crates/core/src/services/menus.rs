//! 메뉴. 위치마다 트리 전체를 한 번에 바꾼다. 깊이는 2단까지.

use std::collections::HashMap;

use serde_json::json;

use crate::actor::{Actor, Permission};
use crate::audit::Target;
use crate::clock;
use crate::error::{Code, Error, Result};
use crate::model::{MENU_LOCATIONS, MenuItem, MenuLink};
use crate::services::required_text;
use crate::slug;
use crate::store::{Ctx, MenuRow, Tx};

const MAX_DEPTH: usize = 2;
const MAX_ITEMS: usize = 50;

pub async fn get(ctx: &Ctx, location: &str) -> Result<Vec<MenuItem>> {
    check_location(location)?;
    Ok(build_tree(ctx.store.menu_rows(location).await?))
}

pub async fn set(
    ctx: &Ctx,
    actor: &Actor,
    location: &str,
    items: Vec<MenuItem>,
) -> Result<Vec<MenuItem>> {
    actor.require(Permission::StructureEdit)?;
    check_location(location)?;
    let items = check(items)?;
    let mut rows = Vec::new();
    let now = clock::now();
    flatten(&items, None, &now, &mut rows);

    let mut tx = Tx::begin(ctx).await?;
    let before = build_tree(tx.menu_rows(location).await?);
    if before == items {
        return Ok(items);
    }
    tx.replace_menu(location, &rows).await?;
    tx.audit(
        actor,
        "menu.set",
        Target::new("menu", location),
        json!({ "before": before, "after": items }),
    )
    .await?;
    tx.commit().await?;
    Ok(items)
}

/// 메뉴 트리를 검사해 저장할 모양(앞뒤 공백 없는 이름, 정리한 주소)으로 돌려준다.
/// 화면 편집기 초안(ADR-0145)도 저장 전에 같은 검사로 그린다.
pub fn check(items: Vec<MenuItem>) -> Result<Vec<MenuItem>> {
    let items = validate(items, 1)?;
    if count(&items) > MAX_ITEMS {
        return Err(Error::new(Code::Validation)
            .with_message("error.menu_too_many")
            .arg("max", MAX_ITEMS.to_string()));
    }
    Ok(items)
}

fn count(items: &[MenuItem]) -> usize {
    items.iter().map(|i| 1 + count(&i.children)).sum()
}

pub fn check_location(location: &str) -> Result<()> {
    if MENU_LOCATIONS.contains(&location) {
        return Ok(());
    }
    Err(Error::new(Code::Validation)
        .with_message("error.menu_location")
        .arg("value", location)
        .arg("allowed", MENU_LOCATIONS.join(", ")))
}

fn validate(items: Vec<MenuItem>, depth: usize) -> Result<Vec<MenuItem>> {
    let mut out = Vec::with_capacity(items.len());
    for item in items {
        if depth > MAX_DEPTH && !item.children.is_empty() {
            return Err(too_deep());
        }
        let label = required_text("label", &item.label, 40)?;
        let link = check_link(item.link)?;
        if depth >= MAX_DEPTH && !item.children.is_empty() {
            return Err(too_deep());
        }
        let children = validate(item.children, depth + 1)?;
        out.push(MenuItem {
            label,
            link,
            children,
        });
    }
    Ok(out)
}

fn too_deep() -> Error {
    Error::new(Code::Validation)
        .with_message("error.menu_depth")
        .arg("max", MAX_DEPTH.to_string())
}

fn check_link(link: MenuLink) -> Result<MenuLink> {
    Ok(match link {
        MenuLink::Home => MenuLink::Home,
        MenuLink::Board { slug } => MenuLink::Board {
            slug: slug::normalize(&slug)?,
        },
        MenuLink::Page { slug } => MenuLink::Page {
            slug: slug::normalize(&slug)?,
        },
        MenuLink::Url { href } => MenuLink::Url {
            href: check_href(&href)?,
        },
    })
}

/// 링크 주소: `https://`, `http://` 또는 사이트 안 경로(`/…`). 위젯 링크도 같은 규칙 (ADR-0059).
pub(crate) fn check_href(href: &str) -> Result<String> {
    let h = href.trim();
    let ok = h.starts_with("https://")
        || h.starts_with("http://")
        || (h.starts_with('/') && !h.starts_with("//") && !h.starts_with("/\\"));
    if !ok || h.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return Err(Error::new(Code::Validation)
            .with_message("error.menu_url")
            .arg("value", href));
    }
    Ok(h.to_owned())
}

fn flatten(items: &[MenuItem], parent: Option<&str>, now: &str, out: &mut Vec<MenuRow>) {
    for (i, item) in items.iter().enumerate() {
        let id = clock::new_id();
        out.push(MenuRow {
            id: id.clone(),
            parent_id: parent.map(str::to_owned),
            label: item.label.clone(),
            link: item.link.clone(),
            sort_order: i64::try_from(i).unwrap_or(i64::MAX),
            created_at: now.to_owned(),
        });
        flatten(&item.children, Some(&id), now, out);
    }
}

fn build_tree(rows: Vec<MenuRow>) -> Vec<MenuItem> {
    let mut by_parent: HashMap<Option<String>, Vec<MenuRow>> = HashMap::new();
    for row in rows {
        by_parent
            .entry(row.parent_id.clone())
            .or_default()
            .push(row);
    }
    fn children(
        parent: Option<String>,
        by_parent: &mut HashMap<Option<String>, Vec<MenuRow>>,
    ) -> Vec<MenuItem> {
        let mut rows = by_parent.remove(&parent).unwrap_or_default();
        rows.sort_by_key(|r| r.sort_order);
        rows.into_iter()
            .map(|r| MenuItem {
                label: r.label,
                link: r.link,
                children: children(Some(r.id), by_parent),
            })
            .collect()
    }
    children(None, &mut by_parent)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(label: &str, children: Vec<MenuItem>) -> MenuItem {
        MenuItem {
            label: label.into(),
            link: MenuLink::Home,
            children,
        }
    }

    #[test]
    fn depth_is_limited() {
        assert!(validate(vec![item("a", vec![item("b", vec![])])], 1).is_ok());
        let deep = vec![item("a", vec![item("b", vec![item("c", vec![])])])];
        assert_eq!(validate(deep, 1).unwrap_err().code(), Code::Validation);
    }

    #[test]
    fn urls_are_checked() {
        for bad in [
            "javascript:alert(1)",
            "//evil.example",
            "/\\evil",
            "ftp://x",
            "a b",
        ] {
            let link = MenuLink::Url { href: bad.into() };
            assert!(check_link(link).is_err(), "{bad}");
        }
        assert!(
            check_link(MenuLink::Url {
                href: "/p/about".into()
            })
            .is_ok()
        );
        assert!(
            check_link(MenuLink::Url {
                href: "https://github.com".into()
            })
            .is_ok()
        );
    }

    #[test]
    fn flatten_and_rebuild_roundtrip() {
        let tree = vec![
            item("a", vec![item("a1", vec![]), item("a2", vec![])]),
            item("b", vec![]),
        ];
        let mut rows = Vec::new();
        flatten(&tree, None, "t", &mut rows);
        assert_eq!(rows.len(), 4);
        assert_eq!(build_tree(rows), tree);
    }
}
