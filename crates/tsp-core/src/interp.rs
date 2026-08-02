//! `${...}` substitution, and the variables it reads.
//!
//! Interpolation happens once, on the generic document, before the pipeline is
//! parsed into its typed form. That ordering is what keeps the rest of the tool
//! unaware that templating exists: by the time a `Stage` is built, its `cmd` is
//! a command and its `deps` are paths.
//!
//! It is also what makes a variable a *tracked* input rather than a hidden one.
//! `tsp.lock` records the command a stage ran and the object id of each
//! dependency, both after substitution — so moving `${data.path}` changes the
//! recorded command or the recorded path, and the stage goes stale by the
//! ordinary rule. Nothing about staleness had to learn what a variable is.
//!
//! An unresolved reference is an error, never a string left as it was found. A
//! `deps` entry still spelled `${train.dataset}` names no file, so nothing
//! would compare it and the stage would report itself current against an input
//! that was never checked.

use serde_json::{Map, Value};

#[derive(Debug, thiserror::Error)]
pub enum InterpError {
    #[error("{path}: {location} refers to {name:?}, which no variable defines")]
    UnknownVariable {
        path: String,
        location: String,
        name: String,
    },
    #[error("{path}: {location} refers to {name:?}, which is {kind} rather than a value")]
    NotAValue {
        path: String,
        location: String,
        name: String,
        kind: &'static str,
    },
    #[error(
        "{path}: {location} is still unresolved after {MAX_DEPTH} passes; a variable refers to itself"
    )]
    TooDeep { path: String, location: String },
}

type Result<T> = std::result::Result<T, InterpError>;

/// How many times a substitution may itself produce a `${...}`.
///
/// A variable whose value names another variable is resolved, which is what
/// lets a params file build one path out of two. A variable naming *itself* is
/// refused rather than looped on.
pub const MAX_DEPTH: usize = 8;

/// The variables in scope for a pipeline.
///
/// A plain JSON object: `${a.b}` is a path into it. Values arrive from
/// `params.yaml`, from the files a `vars:` block names, and from the inline
/// mappings it may carry.
#[derive(Clone, Debug, Default)]
pub struct Vars {
    root: Map<String, Value>,
}

impl Vars {
    pub fn new() -> Self {
        Self::default()
    }

    /// Merges a document's top-level keys, with the later one winning.
    ///
    /// Top-level rather than deep: `vars:` exists to layer whole groups of
    /// settings, and a deep merge would let two files silently combine into a
    /// third that neither states.
    pub fn merge(&mut self, document: &Value) {
        if let Some(object) = document.as_object() {
            for (key, value) in object {
                self.root.insert(key.clone(), value.clone());
            }
        }
    }

    /// Adds a single binding, which is how `${item}` and `${key}` are supplied
    /// while a templated stage is expanded.
    pub fn bind(&mut self, name: &str, value: Value) {
        self.root.insert(name.to_owned(), value);
    }

    pub fn with(&self, name: &str, value: Value) -> Self {
        let mut next = self.clone();
        next.bind(name, value);
        next
    }

    /// Looks up a dotted path, e.g. `train.max_depth` or `models[0].name`.
    pub fn lookup(&self, reference: &str) -> Option<&Value> {
        let mut current: Option<&Value> = None;
        for (i, segment) in segments(reference).enumerate() {
            current = match (i, current, segment) {
                (0, _, Segment::Key(key)) => self.root.get(&key),
                (0, _, Segment::Index(n)) => self.root.get(&n.to_string()),
                (_, Some(Value::Object(map)), Segment::Key(key)) => map.get(&key),
                (_, Some(Value::Array(items)), Segment::Index(n)) => items.get(n),
                (_, Some(Value::Object(map)), Segment::Index(n)) => map.get(&n.to_string()),
                _ => return None,
            };
            current?;
        }
        current
    }
}

enum Segment {
    Key(String),
    Index(usize),
}

/// Splits `a.b[0].c` into its parts. Both spellings are accepted for an index,
/// because DVC writes `${models[0]}` and a params file may equally hold a
/// mapping whose key is a number.
fn segments(reference: &str) -> impl Iterator<Item = Segment> + '_ {
    let mut parts: Vec<Segment> = Vec::new();
    for chunk in reference.split('.') {
        let mut rest = chunk;
        if let Some(open) = rest.find('[') {
            let (name, brackets) = rest.split_at(open);
            if !name.is_empty() {
                parts.push(Segment::Key(name.to_owned()));
            }
            rest = brackets;
            for piece in rest.split('[').skip(1) {
                let index = piece.trim_end_matches(']');
                match index.parse::<usize>() {
                    Ok(n) => parts.push(Segment::Index(n)),
                    Err(_) => parts.push(Segment::Key(index.to_owned())),
                }
            }
        } else if let Ok(n) = rest.parse::<usize>() {
            parts.push(Segment::Index(n));
        } else {
            parts.push(Segment::Key(rest.to_owned()));
        }
    }
    parts.into_iter()
}

/// Renders a resolved value into the text that replaces the reference.
///
/// Only scalars: substituting a mapping into a command would produce whatever
/// the serialiser happened to emit, which is never what the author meant.
fn scalar(value: &Value) -> std::result::Result<String, &'static str> {
    match value {
        Value::String(s) => Ok(s.clone()),
        Value::Bool(b) => Ok(b.to_string()),
        Value::Number(n) => Ok(n.to_string()),
        Value::Null => Ok(String::new()),
        Value::Array(_) => Err("a list"),
        Value::Object(_) => Err("a mapping"),
    }
}

/// Whether the text carries a reference at all, so the common case costs one
/// scan rather than a rebuild.
pub fn has_reference(text: &str) -> bool {
    text.contains("${")
}

/// Substitutes every `${...}` in `text`.
///
/// `location` names where the text came from, for the message; `path` is the
/// pipeline file. A `\${` is a literal, which is how a command that genuinely
/// needs the characters gets them past this.
pub fn render(text: &str, vars: &Vars, path: &str, location: &str) -> Result<String> {
    if !has_reference(text) {
        return Ok(text.to_owned());
    }
    // Termination is "a pass substituted nothing", not "no ${ remains": an
    // escaped reference still contains the characters, so scanning for them
    // would loop on text that is already finished.
    let mut current = text.to_owned();
    for _ in 0..MAX_DEPTH {
        let (next, substituted) = render_once(&current, vars, path, location)?;
        current = next;
        if !substituted {
            return Ok(unescape(&current));
        }
    }
    Err(InterpError::TooDeep {
        path: path.to_owned(),
        location: location.to_owned(),
    })
}

fn unescape(text: &str) -> String {
    text.replace("\\${", "${")
}

fn render_once(text: &str, vars: &Vars, path: &str, location: &str) -> Result<(String, bool)> {
    let bytes = text.as_bytes();
    let mut out = String::with_capacity(text.len());
    let mut substituted = false;
    let mut i = 0;
    while i < bytes.len() {
        // An escaped reference is carried through untouched; unescape runs once
        // at the end, so it cannot be resolved on a later pass either.
        if bytes[i] == b'\\' && text[i..].starts_with("\\${") {
            out.push_str("\\${");
            i += 3;
            continue;
        }
        if text[i..].starts_with("${") {
            if let Some(close) = text[i + 2..].find('}') {
                let name = &text[i + 2..i + 2 + close];
                let value = vars
                    .lookup(name)
                    .ok_or_else(|| InterpError::UnknownVariable {
                        path: path.to_owned(),
                        location: location.to_owned(),
                        name: name.to_owned(),
                    })?;
                let rendered = scalar(value).map_err(|kind| InterpError::NotAValue {
                    path: path.to_owned(),
                    location: location.to_owned(),
                    name: name.to_owned(),
                    kind,
                })?;
                out.push_str(&rendered);
                substituted = true;
                i += 2 + close + 1;
                continue;
            }
        }
        let ch = text[i..].chars().next().expect("in bounds");
        out.push(ch);
        i += ch.len_utf8();
    }
    Ok((out, substituted))
}

/// Substitutes through a whole document, in place.
///
/// Only strings carry references; numbers and booleans are already values.
pub fn render_value(value: &mut Value, vars: &Vars, path: &str, location: &str) -> Result<()> {
    match value {
        Value::String(s) => {
            if has_reference(s) {
                *s = render(s, vars, path, location)?;
            }
        }
        Value::Array(items) => {
            for item in items {
                render_value(item, vars, path, location)?;
            }
        }
        Value::Object(map) => {
            // Keys are rendered too: a stage's `params:` names a file, and an
            // `outs:` entry is a single-key mapping whose key is the path.
            let rendered: Vec<(String, Value)> = map
                .iter()
                .map(|(k, v)| {
                    let key = if has_reference(k) {
                        render(k, vars, path, location)?
                    } else {
                        k.clone()
                    };
                    Ok((key, v.clone()))
                })
                .collect::<Result<_>>()?;
            let mut next = Map::with_capacity(rendered.len());
            for (key, mut item) in rendered {
                render_value(&mut item, vars, path, location)?;
                next.insert(key, item);
            }
            *map = next;
        }
        _ => {}
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn vars() -> Vars {
        let mut vars = Vars::new();
        vars.merge(&json!({
            "train": {"depth": 4, "name": "forest"},
            "models": ["cnn", "rnn"],
            "root": "data",
            "nested": "${root}/prepared",
        }));
        vars
    }

    fn rendered(text: &str) -> String {
        render(text, &vars(), "tsp.yaml", "stage \"train\"").unwrap()
    }

    #[test]
    fn a_dotted_path_resolves_to_its_value() {
        assert_eq!(
            rendered("python train.py --depth ${train.depth}"),
            "python train.py --depth 4"
        );
        assert_eq!(rendered("${train.name}"), "forest");
    }

    #[test]
    fn a_list_is_indexed() {
        assert_eq!(rendered("${models[0]}"), "cnn");
        assert_eq!(rendered("${models[1]}"), "rnn");
    }

    /// A variable naming another is resolved, which is how a params file builds
    /// one path out of two.
    #[test]
    fn a_variable_may_name_another() {
        assert_eq!(rendered("${nested}"), "data/prepared");
    }

    /// The whole point of erroring: a dep still spelled `${...}` names no file,
    /// so nothing would compare it and the stage would report itself current
    /// against an input that was never checked.
    #[test]
    fn an_unknown_variable_is_refused_rather_than_left_in_place() {
        let err = render("${train.missing}", &vars(), "tsp.yaml", "stage \"train\"").unwrap_err();
        let message = err.to_string();
        assert!(message.contains("train.missing"), "{message}");
        assert!(message.contains("no variable defines"), "{message}");
    }

    /// Substituting a mapping into a command yields whatever the serialiser
    /// felt like, which is never what the author meant.
    #[test]
    fn a_mapping_is_not_a_value() {
        let err = render("${train}", &vars(), "tsp.yaml", "stage \"train\"").unwrap_err();
        assert!(err.to_string().contains("mapping"), "{err}");
        let err = render("${models}", &vars(), "tsp.yaml", "stage \"train\"").unwrap_err();
        assert!(err.to_string().contains("a list"), "{err}");
    }

    #[test]
    fn a_self_referring_variable_is_refused_rather_than_looped_on() {
        let mut vars = Vars::new();
        vars.merge(&json!({"a": "${a}"}));
        let err = render("${a}", &vars, "tsp.yaml", "stage \"x\"").unwrap_err();
        assert!(err.to_string().contains("refers to itself"), "{err}");
    }

    /// A command that genuinely needs the characters can have them.
    #[test]
    fn an_escaped_reference_is_left_alone() {
        assert_eq!(rendered("echo \\${HOME}"), "echo ${HOME}");
        assert_eq!(rendered("${root} \\${HOME}"), "data ${HOME}");
    }

    #[test]
    fn text_without_a_reference_is_unchanged() {
        assert_eq!(rendered("python train.py"), "python train.py");
    }

    #[test]
    fn later_files_win_a_merge() {
        let mut vars = Vars::new();
        vars.merge(&json!({"a": 1, "b": 2}));
        vars.merge(&json!({"b": 3}));
        assert_eq!(vars.lookup("a"), Some(&json!(1)));
        assert_eq!(vars.lookup("b"), Some(&json!(3)), "the later file wins");
    }

    /// Keys carry references too: an `outs:` entry is a single-key mapping
    /// whose key is the path.
    #[test]
    fn a_mapping_key_is_rendered() {
        let mut document = json!({"${root}/out.csv": {"cache": false}});
        render_value(&mut document, &vars(), "tsp.yaml", "stage \"train\"").unwrap();
        assert_eq!(document, json!({"data/out.csv": {"cache": false}}));
    }

    #[test]
    fn a_document_is_rendered_through_every_string() {
        let mut document = json!({
            "cmd": "train --depth ${train.depth}",
            "deps": ["${root}/raw.csv", "src/train.py"],
            "n": 3,
        });
        render_value(&mut document, &vars(), "tsp.yaml", "stage \"train\"").unwrap();
        assert_eq!(document["cmd"], "train --depth 4");
        assert_eq!(document["deps"][0], "data/raw.csv");
        assert_eq!(document["deps"][1], "src/train.py");
        assert_eq!(document["n"], 3, "numbers are already values");
    }
}
