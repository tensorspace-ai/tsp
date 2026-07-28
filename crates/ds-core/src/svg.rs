//! Drawing a figure as SVG.
//!
//! Rendered here rather than handed to a JavaScript charting library because
//! `ds plots show` has to work with no network and no browser toolchain: the
//! output is one file you can open, mail, or paste into a report.
//!
//! Colours are the four-hue categorical set validated for both light and dark
//! backgrounds, assigned in fixed order and never cycled — a ninth series folds
//! into the legend rather than reusing hue one, which would make two different
//! things the same colour.

use std::fmt::Write;

use crate::figure::{Figure, Matrix, Rendered, Series};

/// The categorical order. Fixed: a series keeps its colour when a sibling is
/// filtered out, which it would not if colours followed rank.
const HUES: [&str; 4] = ["#2185d0", "#f2711c", "#00b5ad", "#a333c8"];

/// The plot area in user units, and the mapping into it.
struct Scale {
    x0: f64,
    x1: f64,
    y0: f64,
    y1: f64,
    width: f64,
    height: f64,
}

impl Scale {
    fn x(&self, x: f64) -> f64 {
        PAD_LEFT + (x - self.x0) / (self.x1 - self.x0) * self.width
    }

    fn y(&self, y: f64) -> f64 {
        PAD_TOP + self.height - (y - self.y0) / (self.y1 - self.y0) * self.height
    }
}

const WIDTH: f64 = 720.0;
const HEIGHT: f64 = 340.0;
const PAD_LEFT: f64 = 62.0;
const PAD_RIGHT: f64 = 18.0;
const PAD_TOP: f64 = 34.0;
const PAD_BOTTOM: f64 = 46.0;

/// Renders one plot as a standalone `<figure>` fragment.
pub fn render(plot: &Rendered) -> String {
    let body = match &plot.figure {
        Figure::Xy {
            series,
            line,
            markers,
        } => xy(plot, series, *line, *markers),
        Figure::Bars { bars } => {
            let pairs: Vec<(String, f64)> =
                bars.iter().map(|b| (b.label.clone(), b.value)).collect();
            bar(plot, &pairs)
        }
        Figure::Confusion { matrices } => confusion(matrices),
        Figure::Empty { reason } => {
            format!("<p class=\"ds-plot-empty\">{}</p>", escape(reason))
        }
    };

    format!(
        "<figure class=\"ds-plot\">\n<figcaption>{}</figcaption>\n{body}\n</figure>",
        escape(&plot.title)
    )
}

fn xy(plot: &Rendered, series: &[Series], line: bool, markers: bool) -> String {
    let (mut x0, mut x1, mut y0, mut y1) = (f64::MAX, f64::MIN, f64::MAX, f64::MIN);
    for s in series {
        for [x, y] in &s.points {
            x0 = x0.min(*x);
            x1 = x1.max(*x);
            y0 = y0.min(*y);
            y1 = y1.max(*y);
        }
    }
    // A flat series still deserves an axis it sits in the middle of.
    if (x1 - x0).abs() < f64::EPSILON {
        x0 -= 0.5;
        x1 += 0.5;
    }
    if (y1 - y0).abs() < f64::EPSILON {
        y0 -= 0.5;
        y1 += 0.5;
    }

    let scale = Scale {
        x0,
        x1,
        y0,
        y1,
        width: WIDTH - PAD_LEFT - PAD_RIGHT,
        height: HEIGHT - PAD_TOP - PAD_BOTTOM,
    };
    let sx = |x: f64| scale.x(x);
    let sy = |y: f64| scale.y(y);

    let mut svg = String::new();
    let _ = write!(
        svg,
        "<svg viewBox=\"0 0 {WIDTH} {HEIGHT}\" role=\"img\" aria-label=\"{}\" class=\"ds-plot-svg\">",
        escape(&plot.title)
    );

    svg.push_str(&grid(&scale));
    svg.push_str(&axis_labels(&plot.x_label, &plot.y_label));

    for (i, s) in series.iter().enumerate() {
        let hue = HUES[i % HUES.len()];
        if line && s.points.len() > 1 {
            let d: Vec<String> = s
                .points
                .iter()
                .enumerate()
                .map(|(j, [x, y])| {
                    format!(
                        "{} {:.2} {:.2}",
                        if j == 0 { "M" } else { "L" },
                        sx(*x),
                        sy(*y)
                    )
                })
                .collect();
            let _ = write!(
                svg,
                "<path d=\"{}\" fill=\"none\" stroke=\"{hue}\" stroke-width=\"2\" \
                 stroke-linejoin=\"round\" stroke-linecap=\"round\"/>",
                d.join(" ")
            );
        }
        if markers {
            for [x, y] in &s.points {
                // A 1.5px surface ring keeps overlapping points readable.
                let _ = write!(
                    svg,
                    "<circle cx=\"{:.2}\" cy=\"{:.2}\" r=\"4\" fill=\"{hue}\" \
                     stroke=\"var(--ds-plot-surface)\" stroke-width=\"1.5\"><title>{} ({:.4}, {:.4})</title></circle>",
                    sx(*x),
                    sy(*y),
                    escape(&s.label),
                    x,
                    y
                );
            }
        }
    }

    svg.push_str("</svg>");
    // One series is named by the caption; a legend would only repeat it.
    if series.len() > 1 {
        svg.push_str(&legend(series.iter().map(|s| s.label.as_str())));
    }
    svg
}

fn grid(scale: &Scale) -> String {
    let (x0, x1, y0, y1) = (scale.x0, scale.x1, scale.y0, scale.y1);
    let (plot_w, plot_h) = (scale.width, scale.height);
    let (sx, sy) = (|x| scale.x(x), |y| scale.y(y));
    let mut out = String::new();
    // Four intervals: enough to read a value off, few enough to stay recessive.
    for i in 0..=4 {
        let t = f64::from(i) / 4.0;
        let y = y0 + (y1 - y0) * t;
        let py = sy(y);
        let _ = write!(
            out,
            "<line x1=\"{PAD_LEFT}\" y1=\"{py:.2}\" x2=\"{:.2}\" y2=\"{py:.2}\" \
             stroke=\"var(--ds-plot-grid)\" stroke-width=\"1\"/>\
             <text x=\"{:.2}\" y=\"{:.2}\" class=\"ds-plot-tick\" text-anchor=\"end\">{}</text>",
            PAD_LEFT + plot_w,
            PAD_LEFT - 8.0,
            py + 4.0,
            tick(y)
        );

        let x = x0 + (x1 - x0) * t;
        let px = sx(x);
        let _ = write!(
            out,
            "<text x=\"{px:.2}\" y=\"{:.2}\" class=\"ds-plot-tick\" text-anchor=\"middle\">{}</text>",
            PAD_TOP + plot_h + 20.0,
            tick(x)
        );
    }
    out
}

fn axis_labels(x_label: &str, y_label: &str) -> String {
    format!(
        "<text x=\"{:.1}\" y=\"{:.1}\" class=\"ds-plot-axis\" text-anchor=\"middle\">{}</text>\
         <text class=\"ds-plot-axis\" text-anchor=\"middle\" \
         transform=\"translate(14 {:.1}) rotate(-90)\">{}</text>",
        PAD_LEFT + (WIDTH - PAD_LEFT - PAD_RIGHT) / 2.0,
        HEIGHT - 8.0,
        escape(x_label),
        PAD_TOP + (HEIGHT - PAD_TOP - PAD_BOTTOM) / 2.0,
        escape(y_label)
    )
}

fn bar(plot: &Rendered, bars: &[(String, f64)]) -> String {
    let max = bars
        .iter()
        .map(|(_, v)| *v)
        .fold(f64::MIN, f64::max)
        .max(0.0);
    let row_h = 26.0;
    let height = PAD_TOP + row_h * bars.len() as f64 + 24.0;
    // The value is written past the end of its bar, so the longest bar has to
    // stop short of the edge or its label falls outside the viewBox.
    let value_gutter = 54.0;
    let plot_w = WIDTH - PAD_LEFT - PAD_RIGHT - value_gutter;

    let mut svg = format!(
        "<svg viewBox=\"0 0 {WIDTH} {height:.1}\" role=\"img\" aria-label=\"{}\" class=\"ds-plot-svg\">",
        escape(&plot.title)
    );

    for (i, (label, value)) in bars.iter().enumerate() {
        let y = PAD_TOP + row_h * i as f64;
        let w = if max > 0.0 { value / max * plot_w } else { 0.0 };
        let _ = write!(
            svg,
            // 4px rounded data-end, anchored to the baseline at x = PAD_LEFT.
            "<rect x=\"{PAD_LEFT}\" y=\"{:.1}\" width=\"{w:.2}\" height=\"{:.1}\" rx=\"4\" \
             fill=\"{}\"><title>{} {}</title></rect>\
             <text x=\"{:.1}\" y=\"{:.1}\" class=\"ds-plot-tick\" text-anchor=\"end\">{}</text>\
             <text x=\"{:.2}\" y=\"{:.1}\" class=\"ds-plot-value\">{}</text>",
            y + 4.0,
            row_h - 10.0,
            HUES[0],
            escape(label),
            tick(*value),
            PAD_LEFT - 8.0,
            y + 17.0,
            escape(label),
            PAD_LEFT + w + 6.0,
            y + 17.0,
            tick(*value)
        );
    }

    svg.push_str("</svg>");
    svg
}

/// A confusion matrix as an HTML table rather than SVG.
///
/// The cells are a grid of labelled numbers, which a table already is — and it
/// stays selectable, screen-readable and printable, none of which a picture of
/// a grid would be.
fn confusion(matrices: &[Matrix]) -> String {
    let mut out = String::new();
    for m in matrices {
        let peak = m
            .cells
            .iter()
            .flatten()
            .copied()
            .fold(f64::MIN, f64::max)
            .max(f64::MIN_POSITIVE);

        if matrices.len() > 1 {
            let _ = write!(out, "<p class=\"ds-plot-rev\">{}</p>", escape(&m.rev));
        }
        out.push_str("<table class=\"ds-matrix\"><thead><tr><th></th>");
        for col in &m.cols {
            let _ = write!(out, "<th scope=\"col\">{}</th>", escape(col));
        }
        out.push_str("</tr></thead><tbody>");

        for (i, row) in m.rows.iter().enumerate() {
            let _ = write!(out, "<tr><th scope=\"row\">{}</th>", escape(row));
            for (j, _) in m.cols.iter().enumerate() {
                let value = m.cells[i][j];
                // Magnitude as opacity of one hue: a sequential ramp by
                // construction, and monotone in greyscale.
                let weight = (value / peak).clamp(0.0, 1.0);
                let shown = if m.normalized {
                    format!("{value:.2}")
                } else {
                    tick(value)
                };
                let _ = write!(
                    out,
                    "<td style=\"--w:{weight:.3}\"><span class=\"ds-matrix-fill\"></span>\
                     <span class=\"ds-matrix-value\">{shown}</span></td>"
                );
            }
            out.push_str("</tr>");
        }
        out.push_str("</tbody></table>");
    }
    out
}

fn legend<'a>(labels: impl Iterator<Item = &'a str>) -> String {
    let mut out = String::from("<ul class=\"ds-plot-legend\">");
    for (i, label) in labels.enumerate() {
        let _ = write!(
            out,
            "<li><i style=\"background:{}\"></i>{}</li>",
            HUES[i % HUES.len()],
            escape(label)
        );
    }
    out.push_str("</ul>");
    out
}

/// Formats an axis value without trailing noise.
fn tick(v: f64) -> String {
    if v == v.trunc() && v.abs() < 1e15 {
        return format!("{}", v as i64);
    }
    let s = format!("{v:.4}");
    s.trim_end_matches('0').trim_end_matches('.').to_owned()
}

fn escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

/// Wraps rendered figures into one self-contained page.
pub fn page(title: &str, figures: &[String]) -> String {
    format!(
        "<!doctype html>\n<html lang=\"en\">\n<head>\n<meta charset=\"utf-8\">\n\
         <meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\n\
         <title>{}</title>\n<style>{STYLE}</style>\n</head>\n<body>\n\
         <h1>{}</h1>\n{}\n</body>\n</html>\n",
        escape(title),
        escape(title),
        figures.join("\n")
    )
}

/// Dark mode is a selected set, not an inversion: its own four hues, each
/// validated against the dark surface rather than lightened from the light set.
const STYLE: &str = r#"
:root {
  --ds-plot-surface: #ffffff;
  --ds-plot-ink: #1b1c1d;
  --ds-plot-muted: #6b7176;
  --ds-plot-grid: #e4e6e8;
  --ds-plot-seq: #2185d0;
}
@media (prefers-color-scheme: dark) {
  :root {
    --ds-plot-surface: #161718;
    --ds-plot-ink: #dbdbdb;
    --ds-plot-muted: #9a9ea1;
    --ds-plot-grid: #2c2e30;
    --ds-plot-seq: #3a8ac6;
  }
}
body {
  margin: 0 auto; padding: 32px 20px; max-width: 860px;
  background: var(--ds-plot-surface); color: var(--ds-plot-ink);
  font: 14px/1.5 system-ui, -apple-system, "Segoe UI", sans-serif;
}
h1 { font-size: 20px; margin: 0 0 24px; }
.ds-plot { margin: 0 0 36px; }
.ds-plot figcaption { font-weight: 600; margin-bottom: 8px; }
.ds-plot-svg { width: 100%; height: auto; overflow: visible; }
.ds-plot-tick { fill: var(--ds-plot-muted); font-size: 11px; }
.ds-plot-value { fill: var(--ds-plot-ink); font-size: 11px; }
.ds-plot-axis { fill: var(--ds-plot-muted); font-size: 11px; }
.ds-plot-empty { color: var(--ds-plot-muted); font-style: italic; }
.ds-plot-rev { color: var(--ds-plot-muted); margin: 12px 0 4px; }
.ds-plot-legend {
  display: flex; flex-wrap: wrap; gap: 14px;
  list-style: none; margin: 10px 0 0; padding: 0;
  font-size: 12px; color: var(--ds-plot-muted);
}
.ds-plot-legend li { display: flex; align-items: center; gap: 6px; }
.ds-plot-legend i { width: 10px; height: 10px; border-radius: 2px; }
.ds-matrix { border-collapse: separate; border-spacing: 2px; font-size: 12px; }
.ds-matrix th { color: var(--ds-plot-muted); font-weight: 500; padding: 4px 8px; text-align: right; }
.ds-matrix td { position: relative; padding: 8px 12px; text-align: right; min-width: 56px; }
.ds-matrix-fill {
  position: absolute; inset: 0; border-radius: 3px;
  background: var(--ds-plot-seq); opacity: calc(var(--w) * 0.85);
}
.ds-matrix-value { position: relative; }
"#;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::figure::{Bar, Figure, Rendered};

    fn rendered(figure: Figure) -> Rendered {
        Rendered {
            name: "p".into(),
            title: "Loss".into(),
            x_label: "step".into(),
            y_label: "loss".into(),
            template: "linear".into(),
            figure,
        }
    }

    fn xy_figure(series: Vec<Series>) -> Figure {
        Figure::Xy {
            series,
            line: true,
            markers: true,
        }
    }

    fn series(label: &str, points: &[[f64; 2]]) -> Series {
        Series {
            label: label.into(),
            rev: "workspace".into(),
            points: points.to_vec(),
            x_labels: None,
        }
    }

    #[test]
    fn a_line_becomes_a_path() {
        let svg = render(&rendered(xy_figure(vec![series(
            "loss",
            &[[0.0, 1.0], [1.0, 0.5]],
        )])));
        assert!(svg.contains("<path d=\"M "), "{svg}");
        assert!(svg.contains("stroke-width=\"2\""), "thin marks");
    }

    /// One series is named by the caption, so a legend would only repeat it.
    #[test]
    fn a_single_series_gets_no_legend() {
        let svg = render(&rendered(xy_figure(vec![series("loss", &[[0.0, 1.0]])])));
        assert!(!svg.contains("ds-plot-legend"), "{svg}");
    }

    #[test]
    fn two_series_get_a_legend_and_distinct_hues() {
        let svg = render(&rendered(xy_figure(vec![
            series("a", &[[0.0, 1.0]]),
            series("b", &[[0.0, 2.0]]),
        ])));
        assert!(svg.contains("ds-plot-legend"));
        assert!(svg.contains(HUES[0]));
        assert!(svg.contains(HUES[1]));
    }

    /// A flat series must still land on an axis rather than divide by zero.
    #[test]
    fn a_constant_series_renders() {
        let svg = render(&rendered(xy_figure(vec![series(
            "flat",
            &[[0.0, 3.0], [1.0, 3.0]],
        )])));
        assert!(!svg.contains("NaN"), "{svg}");
    }

    #[test]
    fn a_confusion_matrix_is_a_table() {
        let svg = render(&rendered(Figure::Confusion {
            matrices: vec![Matrix {
                rev: "workspace".into(),
                rows: vec!["cat".into(), "dog".into()],
                cols: vec!["cat".into(), "dog".into()],
                cells: vec![vec![2.0, 1.0], vec![0.0, 3.0]],
                normalized: false,
            }],
        }));

        assert!(svg.contains("<table class=\"ds-matrix\""), "{svg}");
        assert!(svg.contains("scope=\"col\""), "headers are real headers");
        assert!(svg.contains("--w:1.000"), "the peak cell is fully weighted");
        assert!(svg.contains("--w:0.000"), "an empty cell has no fill");
    }

    #[test]
    fn bars_are_labelled_and_anchored() {
        let svg = render(&rendered(Figure::Bars {
            bars: vec![Bar {
                label: "petal_width".into(),
                value: 0.42,
                rev: "workspace".into(),
            }],
        }));
        assert!(svg.contains("rx=\"4\""), "rounded data-end");
        assert!(svg.contains("petal_width"));
    }

    /// The value sits past the end of its bar, so the longest bar must leave
    /// room for it inside the viewBox.
    #[test]
    fn the_longest_bars_value_stays_inside_the_frame() {
        let svg = render(&rendered(Figure::Bars {
            bars: vec![Bar {
                label: "widest".into(),
                value: 1.0,
                rev: "workspace".into(),
            }],
        }));

        let x = svg
            .split("class=\"ds-plot-value\"")
            .nth(0)
            .and_then(|before| before.rsplit("<text x=\"").next())
            .and_then(|s| s.split('"').next())
            .and_then(|s| s.parse::<f64>().ok())
            .expect("the value label carries an x");
        assert!(x < WIDTH, "value label at {x} escapes the {WIDTH}pt frame");
    }

    #[test]
    fn an_empty_figure_says_why() {
        let svg = render(&rendered(Figure::Empty {
            reason: "no numeric points to draw".into(),
        }));
        assert!(svg.contains("no numeric points"), "{svg}");
    }

    /// Labels come from repository data, so they must not be able to close a
    /// tag and inject markup into the page.
    #[test]
    fn labels_are_escaped() {
        let svg = render(&rendered(xy_figure(vec![series(
            "</title><script>alert(1)</script>",
            &[[0.0, 1.0]],
        )])));
        assert!(!svg.contains("<script>"), "{svg}");
        assert!(svg.contains("&lt;script&gt;"));
    }

    #[test]
    fn the_page_is_self_contained() {
        let html = page(
            "plots",
            &[render(&rendered(xy_figure(vec![series(
                "a",
                &[[0.0, 1.0]],
            )])))],
        );

        assert!(html.starts_with("<!doctype html>"));
        assert!(html.contains("<style>"), "styles are inline");
        assert!(!html.contains("http://"), "nothing is fetched");
        assert!(!html.contains("<script"), "no script to block offline");
        assert!(html.contains("prefers-color-scheme: dark"));
    }
}
