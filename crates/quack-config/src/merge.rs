//! Deep merge of TOML tables with per-leaf origin tracking.

use std::collections::BTreeMap;

use toml::{Table, Value};

use crate::load::Origin;

pub(crate) type Origins = BTreeMap<String, Origin>;

fn is_append(t: &Table) -> bool {
    t.len() == 1 && matches!(t.get("append"), Some(Value::Array(_)))
}

/// Merge `over` into `base`. Tables merge recursively, scalars and arrays replace,
/// and `{ append = [...] }` extends an existing array.
pub(crate) fn merge(
    base: &mut Table,
    over: Table,
    origin: &Origin,
    origins: &mut Origins,
    prefix: &str,
) {
    for (key, value) in over {
        let path = if prefix.is_empty() {
            key.clone()
        } else {
            format!("{prefix}.{key}")
        };
        match value {
            Value::Table(mut t) if is_append(&t) => {
                let Some(Value::Array(extra)) = t.remove("append") else {
                    continue;
                };
                match base.get_mut(&key) {
                    Some(Value::Array(existing)) => existing.extend(extra),
                    _ => {
                        base.insert(key, Value::Array(extra));
                    }
                }
                origins.insert(path, origin.clone());
            }
            Value::Table(t) => {
                if !matches!(base.get(&key), Some(Value::Table(_))) {
                    base.insert(key.clone(), Value::Table(Table::new()));
                }
                if let Some(Value::Table(b)) = base.get_mut(&key) {
                    merge(b, t, origin, origins, &path);
                }
            }
            v => {
                base.insert(key, v);
                origins.insert(path, origin.clone());
            }
        }
    }
}

pub(crate) fn get_path<'a>(table: &'a Table, path: &str) -> Option<&'a Value> {
    let mut parts = path.split('.');
    let mut cur = table.get(parts.next()?)?;
    for part in parts {
        cur = cur.as_table()?.get(part)?;
    }
    Some(cur)
}

/// Set a dotted path, creating intermediate tables. Fails if a non-table is in the way.
pub(crate) fn set_path(table: &mut Table, path: &str, value: Value) -> bool {
    let mut parts: Vec<&str> = path.split('.').collect();
    let Some(last) = parts.pop() else {
        return false;
    };
    let mut cur = table;
    for part in parts {
        let entry = cur
            .entry(part.to_owned())
            .or_insert_with(|| Value::Table(Table::new()));
        let Value::Table(next) = entry else {
            return false;
        };
        cur = next;
    }
    cur.insert(last.to_owned(), value);
    true
}

/// Remove a dotted path. Returns whether anything was there.
pub(crate) fn remove_path(table: &mut Table, path: &str) -> bool {
    let (parent, last) = match path.rsplit_once('.') {
        Some((p, l)) => (p, l),
        None => return table.remove(path).is_some(),
    };
    let mut cur = table;
    for part in parent.split('.') {
        let Some(Value::Table(next)) = cur.get_mut(part) else {
            return false;
        };
        cur = next;
    }
    cur.remove(last).is_some()
}

/// Flatten a table into `(dotted key, value)` leaves. Arrays are leaves.
pub(crate) fn leaves(table: &Table) -> Vec<(String, Value)> {
    fn walk(prefix: &str, t: &Table, out: &mut Vec<(String, Value)>) {
        for (k, v) in t {
            let path = if prefix.is_empty() {
                k.clone()
            } else {
                format!("{prefix}.{k}")
            };
            match v {
                Value::Table(inner) if !inner.is_empty() => walk(&path, inner, out),
                v => out.push((path, v.clone())),
            }
        }
    }
    let mut out = Vec::new();
    walk("", table, &mut out);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(s: &str) -> Table {
        toml::from_str(s).expect("valid toml")
    }

    #[test]
    fn deep_merge_replace_and_append() {
        let mut base = t("[a]\nx = 1\nlist = [\"a\"]\n[b]\ny = 2");
        let mut origins = Origins::new();
        merge(
            &mut base,
            t("[a]\nx = 5\nlist.append = [\"b\"]"),
            &Origin::Cli,
            &mut origins,
            "",
        );
        assert_eq!(base, t("[a]\nx = 5\nlist = [\"a\", \"b\"]\n[b]\ny = 2"));
        assert_eq!(origins.get("a.x"), Some(&Origin::Cli));
        assert_eq!(origins.get("b.y"), None);
    }

    #[test]
    fn set_and_get_dotted_paths() {
        let mut base = Table::new();
        assert!(set_path(&mut base, "model.default", Value::from("p/m")));
        assert_eq!(get_path(&base, "model.default"), Some(&Value::from("p/m")));
        assert!(!set_path(&mut base, "model.default.x", Value::from(1)));
    }
}
