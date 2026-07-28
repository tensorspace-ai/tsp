//! Plot declarations: what to draw, from which files, with which fields.
//!
//! The format follows DVC's, which spells the same plot several ways — on the
//! artifact inside a stage, or in a top-level `plots:` section that can name a
//! plot and feed it from more than one file. All of them normalise into `Plot`
//! here, so renderers (this crate's SVG one, and Gitea's canvas one) never see
//! the polymorphism.

use indexmap::IndexMap;
use serde::{Deserialize, Serialize};

/// How a plot is drawn. Names match DVC's built-in templates.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Template {
    /// Points joined in row order. The default, as in DVC.
    #[default]
    Linear,
    /// Linear without the point markers.
    Simple,
    Scatter,
    /// Scatter with a small random offset, to separate overlapping points.
    ScatterJitter,
    /// Linear with the series smoothed.
    Smooth,
    /// A matrix of actual against predicted, shaded by count.
    Confusion,
    /// Confusion, with each row scaled to sum to one.
    ConfusionNormalized,
    /// Horizontal bars in the data's own order.
    BarHorizontal,
    /// Horizontal bars ordered by length.
    BarHorizontalSorted,
}

impl Template {
    pub fn parse(name: &str) -> Option<Self> {
        Some(match name {
            "linear" => Self::Linear,
            "simple" => Self::Simple,
            "scatter" => Self::Scatter,
            "scatter_jitter" => Self::ScatterJitter,
            "smooth" => Self::Smooth,
            "confusion" => Self::Confusion,
            "confusion_normalized" => Self::ConfusionNormalized,
            "bar_horizontal" => Self::BarHorizontal,
            "bar_horizontal_sorted" => Self::BarHorizontalSorted,
            _ => return None,
        })
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Linear => "linear",
            Self::Simple => "simple",
            Self::Scatter => "scatter",
            Self::ScatterJitter => "scatter_jitter",
            Self::Smooth => "smooth",
            Self::Confusion => "confusion",
            Self::ConfusionNormalized => "confusion_normalized",
            Self::BarHorizontal => "bar_horizontal",
            Self::BarHorizontalSorted => "bar_horizontal_sorted",
        }
    }

    /// Whether points are drawn as marks only, with no joining line.
    pub fn is_scatter(self) -> bool {
        matches!(self, Self::Scatter | Self::ScatterJitter)
    }

    pub fn is_bar(self) -> bool {
        matches!(self, Self::BarHorizontal | Self::BarHorizontalSorted)
    }

    /// Confusion templates read two categorical fields rather than an axis
    /// pair, so several code paths branch on this rather than on the variant.
    pub fn is_confusion(self) -> bool {
        matches!(self, Self::Confusion | Self::ConfusionNormalized)
    }
}

/// One file feeding a plot, and the fields to take from it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Source {
    pub file: String,
    /// Set when x is read from a different file than y — the shape DVC's
    /// confusion example uses, pairing predictions in one file against actuals
    /// in another. Rows are joined by position, so the files must line up.
    pub x_file: Option<String>,
    /// The x field. `None` plots against the row index, which is what DVC
    /// calls `step`.
    pub x: Option<String>,
    /// The y fields. Empty means "the file's last field", resolved once the
    /// data is read and its columns are known.
    pub y: Vec<String>,
    /// False when the file is delimited and carries no header row.
    pub header: bool,
}

/// A normalised plot declaration.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Plot {
    /// What the plot is called: the data path, or the key a top-level entry
    /// gave it.
    pub name: String,
    pub template: Template,
    pub title: Option<String>,
    pub x_label: Option<String>,
    pub y_label: Option<String>,
    pub sources: Vec<Source>,
}

impl Plot {
    /// Every file the plot reads, in declaration order and without repeats.
    pub fn files(&self) -> Vec<&str> {
        let mut files: Vec<&str> = Vec::new();
        for source in &self.sources {
            for file in [Some(source.file.as_str()), source.x_file.as_deref()]
                .into_iter()
                .flatten()
            {
                if !files.contains(&file) {
                    files.push(file);
                }
            }
        }
        files
    }

    /// The axis caption: the explicit label, else the field name.
    pub fn x_caption(&self) -> String {
        self.x_label.clone().unwrap_or_else(|| {
            self.sources
                .iter()
                .find_map(|s| s.x.clone())
                .unwrap_or_else(|| "step".to_owned())
        })
    }

    pub fn y_caption(&self) -> String {
        self.y_label.clone().unwrap_or_else(|| {
            self.sources
                .iter()
                .flat_map(|s| s.y.clone())
                .next()
                .unwrap_or_default()
        })
    }
}

/// The options a plot entry carries, in either spelling.
#[derive(Clone, Debug, Default, Deserialize)]
pub struct Options {
    #[serde(default)]
    pub template: Option<String>,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub x: Option<AxisSpec>,
    #[serde(default)]
    pub y: Option<AxisSpec>,
    #[serde(default)]
    pub x_label: Option<String>,
    #[serde(default)]
    pub y_label: Option<String>,
    /// Delimited data only: false when the file has no header row.
    #[serde(default)]
    pub header: Option<bool>,
    // Storage options, meaningful only on a stage's own plots. Parsed so a
    // shared entry does not fail on them.
    #[serde(default)]
    pub cache: Option<bool>,
    #[serde(default)]
    pub persist: Option<bool>,
}

/// An axis: one field name, a list of them, or a mapping from file to field.
#[derive(Clone, Debug, Deserialize)]
#[serde(untagged)]
pub enum AxisSpec {
    Field(String),
    Fields(Vec<String>),
    /// `y: {prc.json: precision}` — the form that lets one plot span files.
    PerFile(IndexMap<String, FieldList>),
}

#[derive(Clone, Debug, Deserialize)]
#[serde(untagged)]
pub enum FieldList {
    One(String),
    Many(Vec<String>),
}

impl FieldList {
    fn into_vec(self) -> Vec<String> {
        match self {
            Self::One(f) => vec![f],
            Self::Many(f) => f,
        }
    }
}

impl AxisSpec {
    /// The field to use for a file that the spec does not mention by name.
    fn shared(&self) -> Vec<String> {
        match self {
            Self::Field(f) => vec![f.clone()],
            Self::Fields(f) => f.clone(),
            Self::PerFile(_) => Vec::new(),
        }
    }

    fn per_file(&self) -> IndexMap<String, Vec<String>> {
        match self {
            Self::PerFile(map) => map
                .iter()
                .map(|(file, fields)| (file.clone(), fields.clone().into_vec()))
                .collect(),
            _ => IndexMap::new(),
        }
    }
}

impl Options {
    pub fn template(&self) -> Template {
        self.template
            .as_deref()
            .and_then(Template::parse)
            .unwrap_or_default()
    }
}

/// Builds a plot declared on its own data file, as a stage's `plots:` entry is.
pub fn from_artifact(path: &str, options: &Options) -> Plot {
    let x = options
        .x
        .as_ref()
        .and_then(|a| a.shared().into_iter().next());
    let y = options.y.as_ref().map(AxisSpec::shared).unwrap_or_default();

    Plot {
        name: path.to_owned(),
        template: options.template(),
        title: options.title.clone(),
        x_label: options.x_label.clone(),
        y_label: options.y_label.clone(),
        sources: vec![Source {
            file: path.to_owned(),
            // A stage's plot is declared on its own file, so x always comes
            // from it.
            x_file: None,
            x,
            y,
            header: options.header.unwrap_or(true),
        }],
    }
}

/// Builds a plot from a top-level `plots:` entry, whose key may be a custom
/// name rather than a path.
///
/// When `y` names files, those files are the sources and the key is only a
/// label; otherwise the key is itself the data file.
pub fn from_top_level(name: &str, options: &Options) -> Plot {
    let per_file = options
        .y
        .as_ref()
        .map(AxisSpec::per_file)
        .unwrap_or_default();
    let x_per_file = options
        .x
        .as_ref()
        .map(AxisSpec::per_file)
        .unwrap_or_default();
    let x_shared = options
        .x
        .as_ref()
        .and_then(|a| a.shared().into_iter().next());

    let sources = if per_file.is_empty() {
        // The entry is keyed on its own data file.
        vec![Source {
            file: name.to_owned(),
            x_file: None,
            x: x_shared,
            y: options.y.as_ref().map(AxisSpec::shared).unwrap_or_default(),
            header: options.header.unwrap_or(true),
        }]
    } else {
        per_file
            .into_iter()
            .enumerate()
            .map(|(i, (file, y))| {
                // A file named in both x and y pairs with itself; otherwise the
                // two lists pair by position, which is how x and y in separate
                // files are matched up.
                let (x_file, x) = match x_per_file.get(&file) {
                    Some(fields) => (None, fields.first().cloned()),
                    None => match x_per_file.get_index(i).or_else(|| {
                        (x_per_file.len() == 1)
                            .then(|| x_per_file.get_index(0))
                            .flatten()
                    }) {
                        Some((other, fields)) => (Some(other.clone()), fields.first().cloned()),
                        None => (None, x_shared.clone()),
                    },
                };
                Source {
                    file,
                    x_file,
                    x,
                    y,
                    header: options.header.unwrap_or(true),
                }
            })
            .collect()
    };

    Plot {
        name: name.to_owned(),
        template: options.template(),
        title: options.title.clone(),
        x_label: options.x_label.clone(),
        y_label: options.y_label.clone(),
        sources,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn options(yaml: &str) -> Options {
        yaml_serde::from_str(yaml).unwrap()
    }

    #[test]
    fn templates_round_trip_by_name() {
        for name in [
            "linear",
            "simple",
            "scatter",
            "smooth",
            "confusion",
            "confusion_normalized",
            "bar_horizontal",
        ] {
            assert_eq!(Template::parse(name).unwrap().as_str(), name);
        }
        assert_eq!(Template::parse("nonsense"), None);
    }

    #[test]
    fn an_unknown_template_falls_back_to_linear() {
        assert_eq!(options("template: nonsense").template(), Template::Linear);
        assert_eq!(options("title: x").template(), Template::Linear);
    }

    #[test]
    fn confusion_variants_are_grouped() {
        assert!(Template::Confusion.is_confusion());
        assert!(Template::ConfusionNormalized.is_confusion());
        assert!(!Template::Linear.is_confusion());
    }

    #[test]
    fn a_stage_plot_reads_its_own_file() {
        let plot = from_artifact(
            "plots/confusion.json",
            &options("template: confusion\nx: predicted\ny: actual\ncache: false\n"),
        );

        assert_eq!(plot.name, "plots/confusion.json");
        assert_eq!(plot.template, Template::Confusion);
        assert_eq!(plot.sources.len(), 1);
        assert_eq!(plot.sources[0].file, "plots/confusion.json");
        assert_eq!(plot.sources[0].x.as_deref(), Some("predicted"));
        assert_eq!(plot.sources[0].y, ["actual"]);
    }

    #[test]
    fn a_plot_with_no_axes_named_plots_against_the_row_index() {
        let plot = from_artifact("plots/loss.csv", &options("title: Loss"));
        assert_eq!(plot.sources[0].x, None);
        assert!(plot.sources[0].y.is_empty());
        assert_eq!(plot.x_caption(), "step");
    }

    #[test]
    fn several_y_fields_become_several_series() {
        let plot = from_artifact("p.csv", &options("x: epoch\ny: [train_loss, val_loss]\n"));
        assert_eq!(plot.sources[0].y, ["train_loss", "val_loss"]);
    }

    #[test]
    fn a_top_level_entry_keyed_on_a_path_reads_that_path() {
        let plot = from_top_level("plots/roc.csv", &options("x: fpr\ny: tpr\n"));
        assert_eq!(plot.files(), ["plots/roc.csv"]);
        assert_eq!(plot.sources[0].y, ["tpr"]);
    }

    /// The form that lets one plot span files: the key is only a label.
    #[test]
    fn a_named_top_level_plot_takes_its_files_from_y() {
        let plot = from_top_level(
            "Precision-Recall",
            &options("x: recall\ny:\n  eval/prc.json: precision\n  eval/prc2.json: precision\n"),
        );

        assert_eq!(plot.name, "Precision-Recall");
        assert_eq!(plot.files(), ["eval/prc.json", "eval/prc2.json"]);
        for source in &plot.sources {
            assert_eq!(source.x.as_deref(), Some("recall"));
            assert_eq!(source.y, ["precision"]);
        }
    }

    #[test]
    fn a_file_named_in_both_axes_pairs_with_itself() {
        let plot = from_top_level(
            "ROC vs PRC",
            &options(
                "x:\n  prc.json: recall\n  roc.json: fpr\ny:\n  prc.json: precision\n  roc.json: tpr\n",
            ),
        );

        assert_eq!(plot.sources[0].file, "prc.json");
        assert_eq!(plot.sources[0].x.as_deref(), Some("recall"));
        assert_eq!(
            plot.sources[0].x_file, None,
            "same file, no cross-reference"
        );
        assert_eq!(plot.sources[1].x.as_deref(), Some("fpr"));
    }

    /// DVC's confusion example: predictions in one file, actuals in another.
    #[test]
    fn x_and_y_may_live_in_different_files() {
        let plot = from_top_level(
            "confusion",
            &options(
                "template: confusion\ny:\n  dir/preds.csv: predicted\nx:\n  dir/actual.csv: actual\n",
            ),
        );

        assert_eq!(plot.template, Template::Confusion);
        let source = &plot.sources[0];
        assert_eq!(source.file, "dir/preds.csv");
        assert_eq!(source.y, ["predicted"]);
        assert_eq!(source.x_file.as_deref(), Some("dir/actual.csv"));
        assert_eq!(source.x.as_deref(), Some("actual"));
        assert_eq!(plot.files(), ["dir/preds.csv", "dir/actual.csv"]);
    }

    #[test]
    fn a_headerless_source_is_recorded() {
        let plot = from_artifact("p.csv", &options("header: false\ny: '1'\n"));
        assert!(!plot.sources[0].header);
    }

    #[test]
    fn the_full_template_set_is_recognised() {
        for name in ["scatter_jitter", "bar_horizontal_sorted"] {
            assert_eq!(Template::parse(name).unwrap().as_str(), name);
        }
        assert!(Template::Scatter.is_scatter());
        assert!(Template::ScatterJitter.is_scatter());
        assert!(Template::BarHorizontalSorted.is_bar());
    }

    #[test]
    fn captions_prefer_explicit_labels() {
        let plot = from_artifact("p.csv", &options("x: epoch\ny: loss\nx_label: Epoch\n"));
        assert_eq!(plot.x_caption(), "Epoch");
        assert_eq!(plot.y_caption(), "loss");
    }
}
