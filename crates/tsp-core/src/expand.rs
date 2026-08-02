//! Templated stages: `foreach`, `matrix`, and the `vars` they read.
//!
//! Expansion runs on the generic document, between the shape check and the
//! typed parse, so a `Stage` is only ever built from a stage that really
//! exists. Everything downstream — the lock, the staleness graph, the browser —
//! sees an ordinary pipeline and needs to know nothing about templating.
//!
//! Generated stages are named the way DVC names them, `stage@item`, because a
//! `dvc.yaml` renders here without being migrated and the two must agree about
//! what a stage is called. The name reaches `tsp.lock`, so changing it would
//! stale every generated stage.

use serde_json::{Map, Value};

use crate::interp::{self, Vars};
use crate::pipeline::DEFAULT_PARAMS_FILE;

#[derive(Debug, thiserror::Error)]
pub enum ExpandError {
    #[error("{path}: vars should be a list of files or mappings, but is {got}")]
    VarsShape { path: String, got: String },
    #[error("{path}: vars names {file:?}, which does not exist")]
    VarsFileMissing { path: String, file: String },
    #[error("{path}: cannot parse {file}, named by vars")]
    VarsFileUnreadable { path: String, file: String },
    #[error("{path}: stage {name:?} has foreach but no do, so there is nothing to repeat")]
    ForeachWithoutDo { path: String, name: String },
    #[error("{path}: stage {name:?} has a foreach that is {got}, not a list or a mapping")]
    ForeachShape {
        path: String,
        name: String,
        got: String,
    },
    #[error("{path}: stage {name:?} has a matrix that is {got}, not a mapping of name to list")]
    MatrixShape {
        path: String,
        name: String,
        got: String,
    },
    #[error(
        "{path}: stage {name:?} would generate {count} stages, more than the {MAX_GENERATED} allowed"
    )]
    TooMany {
        path: String,
        name: String,
        count: usize,
    },
    #[error(
        "{path}: stage {name:?} generates {generated:?} more than once; two entries render the same name"
    )]
    DuplicateName {
        path: String,
        name: String,
        generated: String,
    },
    #[error(transparent)]
    Interp(#[from] interp::InterpError),
}

type Result<T> = std::result::Result<T, ExpandError>;

/// How many stages one `foreach` or `matrix` may generate.
///
/// A matrix is a product, so a handful of lists multiplies quickly, and every
/// generated stage is a lock entry and a node in the rendered graph. Refused
/// rather than truncated: half a matrix is a pipeline that silently does not
/// run what it says.
pub const MAX_GENERATED: usize = 1000;

/// Reads a file the pipeline names. `None` when it does not exist.
///
/// Supplied by the caller because the two implementations read from different
/// places: the CLI from the working tree or a revision, a git host from a blob
/// at the commit being viewed.
pub type ReadFile<'a> = &'a dyn Fn(&str) -> Option<Vec<u8>>;

fn describe(value: &Value) -> String {
    match value {
        Value::Null => "nothing".to_owned(),
        Value::Bool(_) => "a true/false value".to_owned(),
        Value::Number(_) => "a number".to_owned(),
        Value::String(s) => format!("the text {s:?}"),
        Value::Array(_) => "a list".to_owned(),
        Value::Object(_) => "a mapping".to_owned(),
    }
}

fn parse_document(raw: &[u8]) -> Option<Value> {
    yaml_serde::from_str(&String::from_utf8_lossy(raw)).ok()
}

/// Collects the variables in scope for a pipeline.
///
/// `params.yaml` is loaded first and without being asked for, which is what DVC
/// does and is why an existing `dvc.yaml` resolves here. A `vars:` entry then
/// layers over it, later winning, and may be either another file or an inline
/// mapping.
///
/// A missing `params.yaml` is not an error — most pipelines have one, but a
/// pipeline that references nothing from it need not. A file `vars:` names
/// explicitly and cannot be read *is* an error: it was asked for.
pub fn build_vars(document: &Value, path: &str, read: ReadFile) -> Result<Vars> {
    let mut vars = Vars::new();

    if let Some(raw) = read(DEFAULT_PARAMS_FILE)
        && let Some(document) = parse_document(&raw)
    {
        vars.merge(&document);
    }

    let Some(declared) = document.get("vars") else {
        return Ok(vars);
    };
    let Some(entries) = declared.as_array() else {
        return Err(ExpandError::VarsShape {
            path: path.to_owned(),
            got: describe(declared),
        });
    };

    for entry in entries {
        match entry {
            Value::String(file) => {
                let raw = read(file).ok_or_else(|| ExpandError::VarsFileMissing {
                    path: path.to_owned(),
                    file: file.clone(),
                })?;
                let document =
                    parse_document(&raw).ok_or_else(|| ExpandError::VarsFileUnreadable {
                        path: path.to_owned(),
                        file: file.clone(),
                    })?;
                vars.merge(&document);
            }
            Value::Object(_) => vars.merge(entry),
            other => {
                return Err(ExpandError::VarsShape {
                    path: path.to_owned(),
                    got: describe(other),
                });
            }
        }
    }
    Ok(vars)
}

/// Expands every templated stage and substitutes through the rest.
///
/// The document is left holding only ordinary stages, so the typed parse that
/// follows never meets a `foreach` at all.
pub fn expand(document: &mut Value, vars: &Vars, path: &str) -> Result<()> {
    if let Some(plots) = document.get_mut("plots") {
        interp::render_value(plots, vars, path, "plots")?;
    }
    // `vars:` has done its job; leaving it would fail the typed parse, which
    // refuses keys it does not define.
    if let Some(root) = document.as_object_mut() {
        root.remove("vars");
    }

    let Some(stages) = document.get("stages").and_then(Value::as_object).cloned() else {
        return Ok(());
    };

    let mut expanded = Map::new();
    for (name, stage) in &stages {
        let location = format!("stage {name:?}");
        let generated = if stage.get("foreach").is_some() {
            expand_foreach(name, stage, vars, path)?
        } else if stage.get("matrix").is_some() {
            expand_matrix(name, stage, vars, path)?
        } else {
            let mut plain = stage.clone();
            interp::render_value(&mut plain, vars, path, &location)?;
            vec![(name.clone(), plain)]
        };

        for (generated_name, body) in generated {
            if expanded.contains_key(&generated_name) {
                return Err(ExpandError::DuplicateName {
                    path: path.to_owned(),
                    name: name.clone(),
                    generated: generated_name,
                });
            }
            expanded.insert(generated_name, body);
        }
    }

    document["stages"] = Value::Object(expanded);
    Ok(())
}

/// The text a value contributes to a generated stage's name.
fn name_part(value: &Value) -> Option<String> {
    match value {
        Value::String(s) => Some(s.clone()),
        Value::Bool(b) => Some(b.to_string()),
        Value::Number(n) => Some(n.to_string()),
        _ => None,
    }
}

fn check_count(name: &str, count: usize, path: &str) -> Result<()> {
    if count > MAX_GENERATED {
        return Err(ExpandError::TooMany {
            path: path.to_owned(),
            name: name.to_owned(),
            count,
        });
    }
    Ok(())
}

/// `foreach:` over a list or a mapping, with the body under `do:`.
///
/// A list binds `${item}` to each element; a mapping binds `${key}` to the key
/// and `${item}` to the value, which is how a stage reads a block of settings
/// per variant.
fn expand_foreach(
    name: &str,
    stage: &Value,
    vars: &Vars,
    path: &str,
) -> Result<Vec<(String, Value)>> {
    let body = stage
        .get("do")
        .ok_or_else(|| ExpandError::ForeachWithoutDo {
            path: path.to_owned(),
            name: name.to_owned(),
        })?;

    // The list itself may be a variable, which is how the set of stages is kept
    // in a params file rather than in the pipeline. Resolved as a whole value
    // before any text rendering: `${models}` names a list, and rendering it
    // into a string is exactly what must not happen.
    let mut over = stage["foreach"].clone();
    match &over {
        Value::String(text) => match strip_reference(text).and_then(|r| vars.lookup(r)) {
            Some(resolved) => over = resolved.clone(),
            None => interp::render_value(&mut over, vars, path, &format!("stage {name:?}"))?,
        },
        _ => interp::render_value(&mut over, vars, path, &format!("stage {name:?}"))?,
    }

    let items: Vec<(String, Value, Option<String>)> = match &over {
        Value::Array(elements) => {
            check_count(name, elements.len(), path)?;
            elements
                .iter()
                .enumerate()
                .map(|(i, element)| {
                    // A scalar names itself; anything else has no reading a
                    // person would recognise, so it is numbered.
                    let label = name_part(element).unwrap_or_else(|| i.to_string());
                    (label, element.clone(), None)
                })
                .collect()
        }
        Value::Object(map) => {
            check_count(name, map.len(), path)?;
            map.iter()
                .map(|(key, value)| (key.clone(), value.clone(), Some(key.clone())))
                .collect()
        }
        other => {
            return Err(ExpandError::ForeachShape {
                path: path.to_owned(),
                name: name.to_owned(),
                got: describe(other),
            });
        }
    };

    let mut out = Vec::with_capacity(items.len());
    for (label, item, key) in items {
        let mut scope = vars.with("item", item);
        if let Some(key) = key {
            scope.bind("key", Value::String(key));
        }
        let generated = format!("{name}@{label}");
        let mut rendered = body.clone();
        interp::render_value(&mut rendered, &scope, path, &format!("stage {generated:?}"))?;
        out.push((generated, rendered));
    }
    Ok(out)
}

/// `matrix:` over a mapping of name to list, generating the cross product.
///
/// Unlike `foreach` the body is the stage itself rather than a `do:` block,
/// which is DVC's shape. `${item.<name>}` reads one coordinate.
fn expand_matrix(
    name: &str,
    stage: &Value,
    vars: &Vars,
    path: &str,
) -> Result<Vec<(String, Value)>> {
    let mut spec = stage["matrix"].clone();
    interp::render_value(&mut spec, vars, path, &format!("stage {name:?}"))?;

    let Some(axes) = spec.as_object() else {
        return Err(ExpandError::MatrixShape {
            path: path.to_owned(),
            name: name.to_owned(),
            got: describe(&spec),
        });
    };

    // Declaration order decides the order of the generated names, so the same
    // matrix always produces the same stage names and the same lock.
    let mut combinations: Vec<Vec<(String, Value)>> = vec![Vec::new()];
    for (axis, values) in axes {
        let values = match values {
            Value::Array(values) => values.clone(),
            // A single value is a list of one, which lets a matrix be narrowed
            // without changing its shape.
            scalar => vec![scalar.clone()],
        };
        let mut next = Vec::with_capacity(combinations.len() * values.len());
        for base in &combinations {
            for value in &values {
                let mut row = base.clone();
                row.push((axis.clone(), value.clone()));
                next.push(row);
            }
        }
        combinations = next;
        check_count(name, combinations.len(), path)?;
    }

    let body = {
        let mut body = stage.clone();
        if let Some(map) = body.as_object_mut() {
            map.remove("matrix");
        }
        body
    };

    let mut out = Vec::with_capacity(combinations.len());
    for row in combinations {
        let label = row
            .iter()
            .map(|(_, value)| name_part(value).unwrap_or_default())
            .collect::<Vec<_>>()
            .join("-");
        let item: Map<String, Value> = row.into_iter().collect();
        let scope = vars.with("item", Value::Object(item));
        let generated = format!("{name}@{label}");
        let mut rendered = body.clone();
        interp::render_value(&mut rendered, &scope, path, &format!("stage {generated:?}"))?;
        out.push((generated, rendered));
    }
    Ok(out)
}

/// `${a.b}` as a whole string is a reference to a value rather than text with
/// one in it, which is how `foreach: ${models}` names a list.
fn strip_reference(text: &str) -> Option<&str> {
    let inner = text.strip_prefix("${")?.strip_suffix('}')?;
    (!inner.contains("${")).then_some(inner)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn no_files(_: &str) -> Option<Vec<u8>> {
        None
    }

    fn expanded(text: &str) -> Value {
        let mut document: Value = yaml_serde::from_str(text).unwrap();
        let vars = build_vars(&document, "tsp.yaml", &no_files).unwrap();
        expand(&mut document, &vars, "tsp.yaml").unwrap();
        document
    }

    fn refused(text: &str) -> ExpandError {
        let mut document: Value = yaml_serde::from_str(text).unwrap();
        let vars = match build_vars(&document, "tsp.yaml", &no_files) {
            Ok(vars) => vars,
            Err(err) => return err,
        };
        expand(&mut document, &vars, "tsp.yaml").unwrap_err()
    }

    fn names(document: &Value) -> Vec<String> {
        document["stages"]
            .as_object()
            .unwrap()
            .keys()
            .cloned()
            .collect()
    }

    #[test]
    fn a_foreach_over_a_list_names_each_stage_after_its_item() {
        let document = expanded(
            "stages:\n  build:\n    foreach: [us, eu]\n    do:\n      cmd: build ${item}\n",
        );
        assert_eq!(names(&document), ["build@us", "build@eu"]);
        assert_eq!(document["stages"]["build@us"]["cmd"], "build us");
        assert_eq!(document["stages"]["build@eu"]["cmd"], "build eu");
    }

    /// A mapping binds both halves: the key names the stage, the value carries
    /// its settings.
    #[test]
    fn a_foreach_over_a_mapping_binds_key_and_item() {
        let document = expanded(
            "stages:\n  train:\n    foreach:\n      uk:\n        level: 1\n      us:\n        level: 2\n    do:\n      cmd: train ${key} --level ${item.level}\n",
        );
        assert_eq!(names(&document), ["train@uk", "train@us"]);
        assert_eq!(document["stages"]["train@uk"]["cmd"], "train uk --level 1");
        assert_eq!(document["stages"]["train@us"]["cmd"], "train us --level 2");
    }

    /// The set of stages can live in a params file rather than the pipeline.
    #[test]
    fn a_foreach_may_name_a_variable() {
        let mut document: Value =
            yaml_serde::from_str("vars:\n  - models: [cnn, rnn]\nstages:\n  train:\n    foreach: ${models}\n    do:\n      cmd: train ${item}\n").unwrap();
        let vars = build_vars(&document, "tsp.yaml", &no_files).unwrap();
        expand(&mut document, &vars, "tsp.yaml").unwrap();
        assert_eq!(names(&document), ["train@cnn", "train@rnn"]);
    }

    #[test]
    fn a_matrix_generates_the_cross_product_in_declaration_order() {
        let document = expanded(
            "stages:\n  train:\n    matrix:\n      model: [cnn, rnn]\n      seed: [1, 2]\n    cmd: train ${item.model} ${item.seed}\n",
        );
        assert_eq!(
            names(&document),
            ["train@cnn-1", "train@cnn-2", "train@rnn-1", "train@rnn-2"]
        );
        assert_eq!(document["stages"]["train@rnn-2"]["cmd"], "train rnn 2");
        // The matrix key itself must not survive into the typed parse.
        assert!(document["stages"]["train@cnn-1"].get("matrix").is_none());
    }

    #[test]
    fn a_matrix_axis_may_be_a_single_value() {
        let document = expanded(
            "stages:\n  t:\n    matrix:\n      model: [a, b]\n      seed: 7\n    cmd: run ${item.model} ${item.seed}\n",
        );
        assert_eq!(names(&document), ["t@a-7", "t@b-7"]);
    }

    /// Outputs are paths, and a generated stage needs its own.
    #[test]
    fn substitution_reaches_outs_and_deps_not_only_the_command() {
        let document = expanded(
            "stages:\n  build:\n    foreach: [us, eu]\n    do:\n      cmd: build ${item}\n      deps:\n        - \"src/${item}.py\"\n      outs:\n        - \"out/${item}.bin\":\n            cache: false\n",
        );
        assert_eq!(document["stages"]["build@us"]["deps"][0], "src/us.py");
        let outs = &document["stages"]["build@us"]["outs"][0];
        assert!(outs.get("out/us.bin").is_some(), "{outs}");
    }

    #[test]
    fn a_plain_stage_is_still_substituted() {
        let mut document: Value = yaml_serde::from_str(
            "vars:\n  - root: data\nstages:\n  prepare:\n    cmd: prep ${root}\n    deps:\n      - \"${root}/raw.csv\"\n",
        )
        .unwrap();
        let vars = build_vars(&document, "tsp.yaml", &no_files).unwrap();
        expand(&mut document, &vars, "tsp.yaml").unwrap();
        assert_eq!(document["stages"]["prepare"]["cmd"], "prep data");
        assert_eq!(document["stages"]["prepare"]["deps"][0], "data/raw.csv");
    }

    /// `vars:` has done its job by the time the typed parse runs, which refuses
    /// keys it does not define.
    #[test]
    fn vars_is_removed_once_it_has_been_read() {
        let document = expanded("vars:\n  - a: 1\nstages:\n  s:\n    cmd: run\n");
        assert!(document.get("vars").is_none(), "{document}");
    }

    #[test]
    fn a_foreach_without_a_do_is_refused() {
        let err = refused("stages:\n  build:\n    foreach: [a]\n");
        assert!(err.to_string().contains("nothing to repeat"), "{err}");
    }

    #[test]
    fn a_foreach_over_a_scalar_is_refused() {
        let err = refused("stages:\n  build:\n    foreach: 3\n    do:\n      cmd: x\n");
        assert!(err.to_string().contains("not a list or a mapping"), "{err}");
    }

    /// Half a matrix is a pipeline that silently does not run what it says, so
    /// an oversized product is refused rather than truncated.
    #[test]
    fn an_oversized_product_is_refused_rather_than_truncated() {
        let axis: Vec<String> = (0..40).map(|n| n.to_string()).collect();
        let text = format!(
            "stages:\n  t:\n    matrix:\n      a: [{}]\n      b: [{}]\n    cmd: run\n",
            axis.join(", "),
            axis.join(", ")
        );
        let err = refused(&text);
        assert!(err.to_string().contains("more than the"), "{err}");
    }

    /// Two items rendering the same name would silently drop one.
    #[test]
    fn two_items_generating_one_name_are_refused() {
        let err = refused("stages:\n  t:\n    foreach: [a, a]\n    do:\n      cmd: run ${item}\n");
        assert!(err.to_string().contains("more than once"), "{err}");
    }

    #[test]
    fn a_vars_file_that_was_asked_for_and_is_missing_is_refused() {
        let err = refused("vars:\n  - missing.yaml\nstages:\n  s:\n    cmd: run\n");
        assert!(err.to_string().contains("does not exist"), "{err}");
    }

    /// params.yaml is loaded without being asked for, which is what DVC does
    /// and is why an existing dvc.yaml resolves here.
    #[test]
    fn params_yaml_is_in_scope_without_being_named() {
        let read =
            |path: &str| (path == DEFAULT_PARAMS_FILE).then(|| b"train:\n  depth: 4\n".to_vec());
        let mut document: Value =
            yaml_serde::from_str("stages:\n  t:\n    cmd: run --depth ${train.depth}\n").unwrap();
        let vars = build_vars(&document, "tsp.yaml", &read).unwrap();
        expand(&mut document, &vars, "tsp.yaml").unwrap();
        assert_eq!(document["stages"]["t"]["cmd"], "run --depth 4");
    }

    /// A later `vars:` entry layers over params.yaml rather than being ignored.
    #[test]
    fn a_vars_entry_wins_over_params_yaml() {
        let read =
            |path: &str| (path == DEFAULT_PARAMS_FILE).then(|| b"train:\n  depth: 4\n".to_vec());
        let mut document: Value = yaml_serde::from_str(
            "vars:\n  - train:\n      depth: 9\nstages:\n  t:\n    cmd: run ${train.depth}\n",
        )
        .unwrap();
        let vars = build_vars(&document, "tsp.yaml", &read).unwrap();
        expand(&mut document, &vars, "tsp.yaml").unwrap();
        assert_eq!(document["stages"]["t"]["cmd"], "run 9");
    }
}
