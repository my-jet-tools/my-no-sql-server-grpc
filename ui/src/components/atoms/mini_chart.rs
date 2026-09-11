use dioxus::prelude::*;

#[derive(Clone, PartialEq)]
pub struct MiniChartSeries {
    pub values: Vec<f64>,
    /// CSS modifier class applied to the line, e.g. "mini-chart__line--in".
    pub class: String,
}

impl MiniChartSeries {
    pub fn new(values: Vec<f64>, class: impl Into<String>) -> Self {
        Self {
            values,
            class: class.into(),
        }
    }
}

/// Reusable small multi-series line chart rendered as a stretchy SVG.
#[component]
pub fn MiniChart(
    series: Vec<MiniChartSeries>,
    max: f64,
    label: String,
    #[props(default = 200.0)] height: f64,
) -> Element {
    let width: f64 = 600.0;
    let pad: f64 = 10.0;

    let len = series.iter().map(|s| s.values.len()).max().unwrap_or(0);
    if len < 2 {
        return rsx! {
            div { class: "mini-chart mini-chart--empty", style: "height: {height}px;",
                span { "Collecting data…" }
            }
        };
    }

    let usable_h = height - 2.0 * pad;
    let max = max.max(1.0);
    let step_x = width / (len - 1) as f64;

    let lines = series.into_iter().map(|s| {
        let points = s
            .values
            .iter()
            .enumerate()
            .map(|(idx, value)| {
                let x = idx as f64 * step_x;
                let y = pad + (1.0 - value / max) * usable_h;
                format!("{:.2},{:.2}", x, y)
            })
            .collect::<Vec<_>>()
            .join(" ");
        let class = s.class;
        rsx! {
            polyline {
                class: "mini-chart__line {class}",
                points: "{points}",
                fill: "none",
            }
        }
    });

    let bottom = height - pad;

    rsx! {
        svg {
            class: "mini-chart",
            view_box: "0 0 {width} {height}",
            preserve_aspect_ratio: "none",
            style: "height: {height}px;",
            line {
                class: "mini-chart__grid",
                x1: "0", y1: "{pad:.2}", x2: "{width}", y2: "{pad:.2}",
            }
            line {
                class: "mini-chart__grid",
                x1: "0", y1: "{bottom:.2}", x2: "{width}", y2: "{bottom:.2}",
            }
            {lines}
            text {
                class: "mini-chart__label",
                x: "{width - 4.0}", y: "{pad + 10.0}",
                text_anchor: "end",
                "{label}"
            }
        }
    }
}
