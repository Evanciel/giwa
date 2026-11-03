//! 줄 단위 비교 (리비전 화면, ADR-0055). 의존성 없이 LCS 로 같음·더함·뺌을 낸다.

use schemars::JsonSchema;
use serde::Serialize;

/// 양쪽 줄 수의 곱이 이보다 크면 계산을 줄이려고 통째로 바뀐 것으로 본다.
const MAX_CELLS: usize = 4_000_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum Op {
    Same,
    Add,
    Del,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
pub struct Line {
    pub op: Op,
    pub text: String,
}

/// 비교 결과와 요약.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
pub struct Diff {
    pub lines: Vec<Line>,
    pub added: usize,
    pub removed: usize,
    /// 너무 커서 줄마다 맞추지 않고 통째로 바꿈으로 냈다
    pub coarse: bool,
}

/// `old` 에서 `new` 로 무엇이 바뀌었나.
pub fn lines(old: &str, new: &str) -> Diff {
    let a: Vec<&str> = old.lines().collect();
    let b: Vec<&str> = new.lines().collect();
    // 앞뒤 같은 줄은 표를 만들지 않고 떼어 둔다.
    let head = a.iter().zip(&b).take_while(|(x, y)| x == y).count();
    let tail = a[head..]
        .iter()
        .rev()
        .zip(b[head..].iter().rev())
        .take_while(|(x, y)| x == y)
        .count();
    let (ma, mb) = (&a[head..a.len() - tail], &b[head..b.len() - tail]);

    let mut out: Vec<Line> = a[..head].iter().map(|t| line(Op::Same, t)).collect();
    let coarse = ma.len().saturating_mul(mb.len()) > MAX_CELLS;
    if coarse {
        out.extend(ma.iter().map(|t| line(Op::Del, t)));
        out.extend(mb.iter().map(|t| line(Op::Add, t)));
    } else {
        out.extend(middle(ma, mb));
    }
    out.extend(a[a.len() - tail..].iter().map(|t| line(Op::Same, t)));
    let added = out.iter().filter(|l| l.op == Op::Add).count();
    let removed = out.iter().filter(|l| l.op == Op::Del).count();
    Diff {
        lines: out,
        added,
        removed,
        coarse,
    }
}

fn line(op: Op, text: &str) -> Line {
    Line {
        op,
        text: text.to_owned(),
    }
}

/// LCS 표를 뒤에서부터 채우고 앞에서부터 따라간다. 뺌을 더함보다 먼저 낸다.
fn middle(a: &[&str], b: &[&str]) -> Vec<Line> {
    let (n, m) = (a.len(), b.len());
    let w = m + 1;
    let mut t = vec![0u32; (n + 1) * w];
    for i in (0..n).rev() {
        for j in (0..m).rev() {
            t[i * w + j] = if a[i] == b[j] {
                t[(i + 1) * w + j + 1] + 1
            } else {
                t[(i + 1) * w + j].max(t[i * w + j + 1])
            };
        }
    }
    let mut out = Vec::with_capacity(n + m);
    let (mut i, mut j) = (0, 0);
    while i < n && j < m {
        if a[i] == b[j] {
            out.push(line(Op::Same, a[i]));
            i += 1;
            j += 1;
        } else if t[(i + 1) * w + j] >= t[i * w + j + 1] {
            out.push(line(Op::Del, a[i]));
            i += 1;
        } else {
            out.push(line(Op::Add, b[j]));
            j += 1;
        }
    }
    out.extend(a[i..].iter().map(|t| line(Op::Del, t)));
    out.extend(b[j..].iter().map(|t| line(Op::Add, t)));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ops(d: &Diff) -> String {
        d.lines
            .iter()
            .map(|l| match l.op {
                Op::Same => format!(" {}", l.text),
                Op::Add => format!("+{}", l.text),
                Op::Del => format!("-{}", l.text),
            })
            .collect::<Vec<_>>()
            .join("|")
    }

    #[test]
    fn same_text_has_no_changes() {
        let d = lines("가\n나", "가\n나");
        assert_eq!((d.added, d.removed), (0, 0));
        assert_eq!(ops(&d), " 가| 나");
    }

    #[test]
    fn insert_delete_and_replace() {
        let d = lines("하나\n둘\n셋\n넷", "하나\n둘둘\n셋\n다섯\n넷");
        assert_eq!(ops(&d), " 하나|-둘|+둘둘| 셋|+다섯| 넷");
        assert_eq!((d.added, d.removed), (2, 1));
    }

    #[test]
    fn empty_sides() {
        assert_eq!(ops(&lines("", "새 줄")), "+새 줄");
        assert_eq!(ops(&lines("옛 줄", "")), "-옛 줄");
        assert!(lines("", "").lines.is_empty());
    }

    #[test]
    fn huge_inputs_fall_back_to_whole_replace() {
        let a = (0..2_100)
            .map(|i| format!("a{i}"))
            .collect::<Vec<_>>()
            .join("\n");
        let b = (0..2_100)
            .map(|i| format!("b{i}"))
            .collect::<Vec<_>>()
            .join("\n");
        let d = lines(&format!("머리\n{a}"), &format!("머리\n{b}"));
        assert!(d.coarse);
        assert_eq!((d.added, d.removed), (2_100, 2_100));
        assert_eq!(d.lines[0], line(Op::Same, "머리"));
    }
}
