//! Turning a plot declaration plus its data into something drawable.
//!
//! This is the step between "the pipeline says draw a confusion matrix from
//! these two fields" and any particular renderer. Keeping it separate means the
//! SVG writer here and the canvas one in Gitea agree on what a series is,
//! which revision it belongs to, and how a matrix was counted.

use std::collections::BTreeMap;

use serde::Serialize;
use serde_json::Value;

use crate::plotdata::{self, Row};
use crate::plots::{Plot, Source, Template};

/// Data for one plot, keyed by revision then by file.
///
/// A revision is whatever the caller is comparing — `workspace`, `HEAD`, a
/// branch, an experiment. Order is preserved: it decides series colour.
pub type Loaded = Vec<(String, BTreeMap<String, Vec<Row>>)>;

/// One line, or one scatter cloud.
#[derive(Clone, Debug, Serialize)]
pub struct Series {
    /// What the legend shows.
    pub label: String,
    /// The revision this came from, so a renderer can colour by revision
    /// rather than by series when several revisions are compared.
    pub rev: String,
    pub points: Vec<[f64; 2]>,
    /// Point labels, when x was categorical rather than numeric.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub x_labels: Option<Vec<String>>,
}

/// A counted confusion matrix.
#[derive(Clone, Debug, Serialize)]
pub struct Matrix {
    pub rev: String,
    /// Actual classes, one per row.
    pub rows: Vec<String>,
    /// Predicted classes, one per column.
    pub cols: Vec<String>,
    /// `cells[row][col]`, counts or row-normalised fractions.
    pub cells: Vec<Vec<f64>>,
    pub normalized: bool,
}

/// A plot ready to draw.
#[derive(Clone, Debug, Serialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum Figure {
    /// Lines, points, or both.
    Xy {
        series: Vec<Series>,
        /// Points are joined in row order.
        line: bool,
        /// Point markers are drawn.
        markers: bool,
    },
    /// Horizontal bars, one per record.
    Bars { bars: Vec<Bar> },
    /// One matrix per revision, so a comparison shows them side by side.
    Confusion { matrices: Vec<Matrix> },
    /// The plot named files, fields or a shape that yielded nothing to draw.
    Empty { reason: String },
}

#[derive(Clone, Debug, Serialize)]
pub struct Bar {
    pub label: String,
    pub value: f64,
    pub rev: String,
}

/// Everything a renderer needs for one plot.
#[derive(Clone, Debug, Serialize)]
pub struct Rendered {
    pub name: String,
    pub title: String,
    pub x_label: String,
    pub y_label: String,
    pub template: String,
    pub figure: Figure,
}

/// Builds the drawable form of `plot` from data already read.
pub fn build(plot: &Plot, loaded: &Loaded) -> Rendered {
    let figure = if plot.template.is_confusion() {
        confusion(plot, loaded)
    } else if plot.template.is_bar() {
        bars(plot, loaded)
    } else {
        xy(plot, loaded)
    };

    Rendered {
        name: plot.name.clone(),
        title: plot.title.clone().unwrap_or_else(|| plot.name.clone()),
        x_label: plot.x_caption(),
        y_label: plot.y_caption(),
        template: plot.template.as_str().to_owned(),
        figure,
    }
}

/// The y fields to draw for a source, falling back to the file's last field.
fn y_fields(source: &Source, rows: &[Row]) -> Vec<String> {
    if !source.y.is_empty() {
        return source.y.clone();
    }
    plotdata::default_y_field(rows).into_iter().collect()
}

/// Reads a field as a number, or `None` when it is not one.
fn number(row: &Row, field: &str) -> Option<f64> {
    match row.get(field)? {
        Value::Number(n) => n.as_f64(),
        // CSV cells arrive as text when they hold anything non-numeric; a
        // numeric-looking one is still worth plotting.
        Value::String(s) => s.parse().ok(),
        _ => None,
    }
}

fn text(row: &Row, field: &str) -> Option<String> {
    match row.get(field)? {
        Value::String(s) => Some(s.clone()),
        Value::Null => None,
        other => Some(other.to_string()),
    }
}

fn xy(plot: &Plot, loaded: &Loaded) -> Figure {
    let mut series = Vec::new();
    let multi_rev = loaded.len() > 1;

    for (rev, files) in loaded {
        for source in &plot.sources {
            let Some(rows) = files.get(&source.file) else {
                continue;
            };
            // x may live in a different file, joined by position.
            let x_rows = match &source.x_file {
                Some(other) => files.get(other),
                None => Some(rows),
            };

            for field in y_fields(source, rows) {
                let mut points = Vec::new();
                for (i, row) in rows.iter().enumerate() {
                    let Some(y) = number(row, &field) else {
                        continue;
                    };
                    let x = match (&source.x, x_rows) {
                        (Some(name), Some(xr)) => {
                            xr.get(i).and_then(|r| number(r, name)).unwrap_or(i as f64)
                        }
                        // No x named: the row index, which DVC calls step.
                        _ => i as f64,
                    };
                    points.push([x, y]);
                }
                if points.is_empty() {
                    continue;
                }
                series.push(Series {
                    label: label_for(plot, source, &field, rev, multi_rev),
                    rev: rev.clone(),
                    points,
                    x_labels: None,
                });
            }
        }
    }

    if series.is_empty() {
        return Figure::Empty {
            reason: "no numeric points to draw".to_owned(),
        };
    }
    Figure::Xy {
        series,
        line: !plot.template.is_scatter(),
        markers: plot.template != Template::Simple,
    }
}

/// Names a series without repeating what is already obvious.
///
/// One file and one field means the revision is the only thing that
/// distinguishes series, so that is all the legend needs to say.
fn label_for(plot: &Plot, source: &Source, field: &str, rev: &str, multi_rev: bool) -> String {
    let one_source = plot.sources.len() == 1;
    let one_field = plot.sources.iter().all(|s| s.y.len() <= 1);
    let file = &source.file[common_dir_prefix(&plot.files()).len()..];

    let own = match (one_source, one_field) {
        (true, true) => String::new(),
        (true, false) => field.to_owned(),
        (false, true) => file.to_owned(),
        (false, false) => format!("{file}::{field}"),
    };

    match (multi_rev, own.is_empty()) {
        (true, true) => rev.to_owned(),
        (true, false) => format!("{rev} · {own}"),
        (false, true) => field.to_owned(),
        (false, false) => own,
    }
}

/// The directory prefix every source shares, which says nothing about which
/// series is which.
fn common_dir_prefix(files: &[&str]) -> String {
    if files.len() < 2 {
        return String::new();
    }
    let segments: Vec<&str> = files[0].split('/').collect();

    let mut shared = 0;
    while shared + 1 < segments.len() {
        let candidate = format!("{}/", segments[..=shared].join("/"));
        if !files.iter().all(|f| f.starts_with(&candidate)) {
            break;
        }
        shared += 1;
    }
    match shared {
        0 => String::new(),
        n => format!("{}/", segments[..n].join("/")),
    }
}

fn bars(plot: &Plot, loaded: &Loaded) -> Figure {
    let mut bars = Vec::new();

    for (rev, files) in loaded {
        for source in &plot.sources {
            let Some(rows) = files.get(&source.file) else {
                continue;
            };
            for field in y_fields(source, rows) {
                for (i, row) in rows.iter().enumerate() {
                    let Some(value) = number(row, &field) else {
                        continue;
                    };
                    let label = source
                        .x
                        .as_ref()
                        .and_then(|name| text(row, name))
                        .unwrap_or_else(|| i.to_string());
                    bars.push(Bar {
                        label,
                        value,
                        rev: rev.clone(),
                    });
                }
            }
        }
    }

    if bars.is_empty() {
        return Figure::Empty {
            reason: "no values to draw".to_owned(),
        };
    }
    if plot.template == Template::BarHorizontalSorted {
        bars.sort_by(|a, b| b.value.total_cmp(&a.value));
    }
    Figure::Bars { bars }
}

/// Counts a confusion matrix per revision.
///
/// Each row of the data is one observation: its x field is the predicted class
/// and its y field the actual one. A cell is how many observations fell into
/// that pair — the data is not pre-aggregated, which is what lets the same file
/// also serve as a scatter or a table.
fn confusion(plot: &Plot, loaded: &Loaded) -> Figure {
    let mut matrices = Vec::new();

    for (rev, files) in loaded {
        let mut counts: BTreeMap<(String, String), f64> = BTreeMap::new();
        let mut rows_seen: Vec<String> = Vec::new();
        let mut cols_seen: Vec<String> = Vec::new();

        for source in &plot.sources {
            let Some(rows) = files.get(&source.file) else {
                continue;
            };
            let x_rows = match &source.x_file {
                Some(other) => files.get(other),
                None => Some(rows),
            };
            let (Some(x_field), Some(x_rows)) = (source.x.as_deref(), x_rows) else {
                continue;
            };

            for field in y_fields(source, rows) {
                for (i, row) in rows.iter().enumerate() {
                    let (Some(actual), Some(predicted)) = (
                        text(row, &field),
                        x_rows.get(i).and_then(|r| text(r, x_field)),
                    ) else {
                        continue;
                    };
                    if !rows_seen.contains(&actual) {
                        rows_seen.push(actual.clone());
                    }
                    if !cols_seen.contains(&predicted) {
                        cols_seen.push(predicted.clone());
                    }
                    *counts.entry((actual, predicted)).or_default() += 1.0;
                }
            }
        }

        if counts.is_empty() {
            continue;
        }

        // Both axes list the same classes: a class predicted but never actual
        // still needs its column, and the matrix reads as square.
        let mut classes = rows_seen.clone();
        for col in &cols_seen {
            if !classes.contains(col) {
                classes.push(col.clone());
            }
        }
        classes.sort();

        let normalized = plot.template == Template::ConfusionNormalized;
        let cells: Vec<Vec<f64>> = classes
            .iter()
            .map(|actual| {
                let raw: Vec<f64> = classes
                    .iter()
                    .map(|predicted| {
                        counts
                            .get(&(actual.clone(), predicted.clone()))
                            .copied()
                            .unwrap_or(0.0)
                    })
                    .collect();
                if !normalized {
                    return raw;
                }
                // Row-normalised: each actual class sums to one, so classes
                // with few examples are still readable.
                let total: f64 = raw.iter().sum();
                if total == 0.0 {
                    raw
                } else {
                    raw.into_iter().map(|c| c / total).collect()
                }
            })
            .collect();

        matrices.push(Matrix {
            rev: rev.clone(),
            rows: classes.clone(),
            cols: classes,
            cells,
            normalized,
        });
    }

    if matrices.is_empty() {
        return Figure::Empty {
            reason: "no observations to count; a confusion plot needs both x and y fields"
                .to_owned(),
        };
    }
    Figure::Confusion { matrices }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plots;

    fn rows(json: &str) -> Vec<Row> {
        plotdata::parse("p.json", json.as_bytes()).unwrap()
    }

    fn one_rev(file: &str, rows: Vec<Row>) -> Loaded {
        vec![(
            "workspace".to_owned(),
            BTreeMap::from([(file.to_owned(), rows)]),
        )]
    }

    fn options(yaml: &str) -> plots::Options {
        yaml_serde::from_str(yaml).unwrap()
    }

    #[test]
    fn a_linear_plot_uses_the_row_index_when_no_x_is_named() {
        let plot = plots::from_artifact("p.json", &options("y: loss"));
        let data = one_rev("p.json", rows(r#"[{"loss": 0.9}, {"loss": 0.5}]"#));

        let Figure::Xy { series, line, .. } = build(&plot, &data).figure else {
            panic!("expected an xy figure");
        };
        assert!(line);
        assert_eq!(series[0].points, [[0.0, 0.9], [1.0, 0.5]]);
    }

    #[test]
    fn a_named_x_field_is_used() {
        let plot = plots::from_artifact("p.json", &options("x: epoch\ny: loss"));
        let data = one_rev(
            "p.json",
            rows(r#"[{"epoch": 10, "loss": 0.9}, {"epoch": 20, "loss": 0.5}]"#),
        );

        let Figure::Xy { series, .. } = build(&plot, &data).figure else {
            panic!()
        };
        assert_eq!(series[0].points, [[10.0, 0.9], [20.0, 0.5]]);
    }

    #[test]
    fn y_defaults_to_the_last_field() {
        let plot = plots::from_artifact("p.json", &options("title: t"));
        let data = one_rev("p.json", rows(r#"[{"epoch": 1, "acc": 0.4}]"#));

        let Figure::Xy { series, .. } = build(&plot, &data).figure else {
            panic!()
        };
        assert_eq!(series[0].points, [[0.0, 0.4]]);
    }

    #[test]
    fn several_y_fields_become_several_series() {
        let plot = plots::from_artifact("p.json", &options("x: epoch\ny: [train, val]"));
        let data = one_rev(
            "p.json",
            rows(r#"[{"epoch": 1, "train": 0.9, "val": 0.8}]"#),
        );

        let Figure::Xy { series, .. } = build(&plot, &data).figure else {
            panic!()
        };
        assert_eq!(series.len(), 2);
        assert_eq!(series[0].label, "train");
        assert_eq!(series[1].label, "val");
    }

    /// With one file and one field, the revision is the only thing telling
    /// series apart, so it is the whole label.
    #[test]
    fn comparing_revisions_labels_series_by_revision() {
        let plot = plots::from_artifact("p.json", &options("y: loss"));
        let data: Loaded = vec![
            (
                "workspace".to_owned(),
                BTreeMap::from([("p.json".to_owned(), rows(r#"[{"loss": 0.5}]"#))]),
            ),
            (
                "HEAD".to_owned(),
                BTreeMap::from([("p.json".to_owned(), rows(r#"[{"loss": 0.9}]"#))]),
            ),
        ];

        let Figure::Xy { series, .. } = build(&plot, &data).figure else {
            panic!()
        };
        assert_eq!(series.len(), 2);
        assert_eq!(series[0].label, "workspace");
        assert_eq!(series[1].label, "HEAD");
        assert_eq!(series[1].rev, "HEAD");
    }

    #[test]
    fn scatter_drops_the_joining_line() {
        let plot = plots::from_artifact("p.json", &options("template: scatter\ny: v"));
        let data = one_rev("p.json", rows(r#"[{"v": 1}]"#));

        let Figure::Xy { line, markers, .. } = build(&plot, &data).figure else {
            panic!()
        };
        assert!(!line);
        assert!(markers);
    }

    #[test]
    fn simple_drops_the_markers() {
        let plot = plots::from_artifact("p.json", &options("template: simple\ny: v"));
        let data = one_rev("p.json", rows(r#"[{"v": 1}]"#));

        let Figure::Xy { markers, .. } = build(&plot, &data).figure else {
            panic!()
        };
        assert!(!markers);
    }

    #[test]
    fn a_confusion_matrix_counts_pairs() {
        let plot = plots::from_artifact(
            "p.json",
            &options("template: confusion\nx: predicted\ny: actual"),
        );
        let data = one_rev(
            "p.json",
            rows(
                r#"[{"actual": "cat", "predicted": "cat"},
                    {"actual": "cat", "predicted": "dog"},
                    {"actual": "dog", "predicted": "dog"},
                    {"actual": "cat", "predicted": "cat"}]"#,
            ),
        );

        let Figure::Confusion { matrices } = build(&plot, &data).figure else {
            panic!("expected a confusion figure")
        };
        let m = &matrices[0];
        assert_eq!(m.rows, ["cat", "dog"]);
        // cat predicted cat twice, cat predicted dog once, dog predicted dog once.
        assert_eq!(m.cells[0], [2.0, 1.0]);
        assert_eq!(m.cells[1], [0.0, 1.0]);
        assert!(!m.normalized);
    }

    #[test]
    fn a_normalized_matrix_has_rows_summing_to_one() {
        let plot = plots::from_artifact(
            "p.json",
            &options("template: confusion_normalized\nx: predicted\ny: actual"),
        );
        let data = one_rev(
            "p.json",
            rows(
                r#"[{"actual": "cat", "predicted": "cat"},
                    {"actual": "cat", "predicted": "dog"}]"#,
            ),
        );

        let Figure::Confusion { matrices } = build(&plot, &data).figure else {
            panic!()
        };
        assert_eq!(matrices[0].cells[0], [0.5, 0.5]);
        assert!(matrices[0].normalized);
    }

    /// A class that is only ever predicted still needs a column, or the matrix
    /// silently loses the mistakes made against it.
    #[test]
    fn a_class_that_is_only_predicted_still_gets_an_axis_entry() {
        let plot = plots::from_artifact(
            "p.json",
            &options("template: confusion\nx: predicted\ny: actual"),
        );
        let data = one_rev("p.json", rows(r#"[{"actual": "cat", "predicted": "fox"}]"#));

        let Figure::Confusion { matrices } = build(&plot, &data).figure else {
            panic!()
        };
        assert_eq!(matrices[0].rows, ["cat", "fox"]);
        assert_eq!(matrices[0].cols, ["cat", "fox"]);
    }

    #[test]
    fn bars_may_be_sorted_by_length() {
        let plot = plots::from_artifact(
            "p.json",
            &options("template: bar_horizontal_sorted\nx: feature\ny: weight"),
        );
        let data = one_rev(
            "p.json",
            rows(r#"[{"feature": "a", "weight": 1}, {"feature": "b", "weight": 5}]"#),
        );

        let Figure::Bars { bars } = build(&plot, &data).figure else {
            panic!("expected bars")
        };
        assert_eq!(bars[0].label, "b");
        assert_eq!(bars[0].value, 5.0);
    }

    #[test]
    fn a_plot_whose_field_is_absent_reports_rather_than_drawing_nothing() {
        let plot = plots::from_artifact("p.json", &options("y: missing"));
        let data = one_rev("p.json", rows(r#"[{"loss": 1}]"#));

        let Figure::Empty { reason } = build(&plot, &data).figure else {
            panic!("expected an empty figure")
        };
        assert!(reason.contains("numeric"), "{reason}");
    }

    /// A shared directory says nothing about which series is which.
    #[test]
    fn series_labels_drop_the_prefix_every_file_shares() {
        assert_eq!(common_dir_prefix(&["plots/a.csv", "plots/b.csv"]), "plots/");
        assert_eq!(
            common_dir_prefix(&["eval/train/roc.json", "eval/test/roc.json"]),
            "eval/"
        );
        // Nothing in common, and a lone file, both keep the whole path.
        assert_eq!(common_dir_prefix(&["a/x.csv", "b/x.csv"]), "");
        assert_eq!(common_dir_prefix(&["plots/only.csv"]), "");
    }

    #[test]
    fn the_title_falls_back_to_the_plot_name() {
        let plot = plots::from_artifact("plots/loss.csv", &options("y: v"));
        let data = one_rev("plots/loss.csv", rows(r#"[{"v": 1}]"#));
        assert_eq!(build(&plot, &data).title, "plots/loss.csv");
    }
}
