use std::time::Instant;

use gpui_kit::base::Spring;
use gpui_kit::base::motion::spring;
use gpui_kit::component::ActiveTheme;
use gpui_kit::component::plot::label::{TEXT_GAP, TEXT_SIZE, Text};
use gpui_kit::component::plot::scale::{Scale, ScaleLinear};
use gpui_kit::component::plot::shape::Line;
use gpui_kit::component::plot::tooltip::{CrossLine, Dot, PlotHover, Tooltip, TooltipState};
use gpui_kit::component::plot::{Grid, IntoPlot, PathCaches, Plot, PlotLabel, StrokeStyle};
use gpui_kit::*;

use crate::ui::format_speed;

use super::speed_history::{SPEED_HISTORY_SPAN, SpeedHistory};

/// The size of the dot marking a hovered data point; mirrors gpui-component's
/// own chart dot size, which isn't exposed for reuse outside its crate.
const HOVER_DOT_SIZE: Pixels = px(8.);
/// The ring behind a hovered dot, growing out of the dot as the hover fades in.
const HOVER_HALO_SIZE: f32 = 20.;
/// Exponential smoothing applied to the plotted curve only, not the raw
/// numbers shown as text: DepotDownloader's per-sample speed is bursty
/// enough that a spline through the raw values overshoots into visual noise
/// instead of the trend the numbers actually describe.
const CHART_SMOOTHING: f64 = 0.25;
/// Left margin reserved for the y-axis value labels.
const Y_AXIS_GUTTER: f32 = 56.;
const KIB: f64 = 1024.0;
const MIB: f64 = KIB * 1024.0;
const GIB: f64 = MIB * 1024.0;

/// The display unit a value of this magnitude would be shown in (matching
/// `format_bytes`'s own thresholds), so axis bounds snap to round numbers in
/// the same unit the label text ends up in (round bytes are not round MB).
fn axis_unit_bytes(value: f64) -> f64 {
    if value >= GIB {
        GIB
    } else if value >= MIB {
        MIB
    } else if value >= KIB {
        KIB
    } else {
        1.0
    }
}

/// Rounds `rough` up to the nearest "nice" number: 1, 2, 5 or 10 times a
/// power of ten. Used to turn an arbitrary axis step into one a person would
/// actually write on a ruler.
fn nice_step(rough: f64) -> f64 {
    let rough = rough.max(f64::MIN_POSITIVE);
    let magnitude = 10f64.powf(rough.log10().floor());
    let fraction = rough / magnitude;
    let nice_fraction = if fraction <= 1.0 {
        1.0
    } else if fraction <= 2.0 {
        2.0
    } else if fraction <= 5.0 {
        5.0
    } else {
        10.0
    };
    nice_fraction * magnitude
}

fn ema(previous: &mut Option<f64>, value: f64) -> f64 {
    let smoothed = match *previous {
        Some(prev) => prev + CHART_SMOOTHING * (value - prev),
        None => value,
    };
    *previous = Some(smoothed);
    smoothed
}

#[derive(Clone, Copy)]
struct SpeedPoint {
    seconds_ago: f64,
    download_bytes_per_sec: f64,
    disk_bytes_per_sec: f64,
}

/// Where the download (if shown) and disk hover dots have slid to; sampled
/// once per frame in [`Plot::hover`].
#[derive(Clone, Copy)]
struct ChartHover {
    download_dot: Option<Point<Pixels>>,
    disk_dot: Point<Pixels>,
    focus: f32,
}

#[derive(IntoPlot)]
pub struct SpeedChart {
    data: Vec<SpeedPoint>,
    download_stroke: Hsla,
    disk_stroke: Hsla,
    show_download: bool,
    id: Option<ElementId>,
    hover: Option<ChartHover>,
}

impl SpeedChart {
    /// `now` anchors the "elapsed seconds ago" of every sample: the caller
    /// passes a frozen instant while the chart is hovered so it stops
    /// scrolling under the cursor, and the live instant otherwise. Samples
    /// newer than `now` are dropped rather than clamped to it, so a freeze
    /// shows an exact snapshot instead of every sample collected since
    /// piling up at the same point once `now` falls behind the wall clock.
    pub fn new(history: &SpeedHistory, now: Instant) -> Self {
        let mut smoothed_download = None;
        let mut smoothed_disk = None;
        let data = history
            .samples()
            .iter()
            .filter(|sample| sample.at <= now)
            .map(|sample| SpeedPoint {
                seconds_ago: now.duration_since(sample.at).as_secs_f64(),
                download_bytes_per_sec: ema(&mut smoothed_download, sample.download_bytes_per_sec),
                disk_bytes_per_sec: ema(&mut smoothed_disk, sample.disk_bytes_per_sec),
            })
            .collect();
        Self {
            data,
            download_stroke: transparent_black(),
            disk_stroke: transparent_black(),
            show_download: true,
            id: None,
            hover: None,
        }
    }

    /// Enables an interactive hover tooltip. The `id` must be unique among sibling elements.
    pub fn id(mut self, id: impl Into<ElementId>) -> Self {
        self.id = Some(id.into());
        self
    }

    pub fn download_stroke(mut self, stroke: impl Into<Hsla>) -> Self {
        self.download_stroke = stroke.into();
        self
    }

    pub fn disk_stroke(mut self, stroke: impl Into<Hsla>) -> Self {
        self.disk_stroke = stroke.into();
        self
    }

    /// Whether the download line, dot and tooltip row are drawn. Off while
    /// verifying, when DepotDownloader isn't downloading and the download
    /// speed is meaningless.
    pub fn show_download(mut self, show_download: bool) -> Self {
        self.show_download = show_download;
        self
    }

    /// Builds the x (elapsed seconds, newest at the right, offset by the
    /// y-axis label gutter) and y (bytes/sec, shared by both series) scales
    /// for the given bounds.
    fn scales(&self, bounds: Bounds<Pixels>, y_bounds: (f64, f64)) -> (ScaleLinear<f64>, ScaleLinear<f64>) {
        let width = bounds.size.width.as_f32();
        let height = bounds.size.height.as_f32();

        let x = ScaleLinear::new(
            vec![0.0, SPEED_HISTORY_SPAN.as_secs_f64()],
            vec![width, Y_AXIS_GUTTER],
        );
        let y = ScaleLinear::new(vec![y_bounds.0, y_bounds.1], vec![height, 4.]);

        (x, y)
    }

    /// The y-axis bottom and top: the visible window's own minimum and
    /// maximum, floored and ceiled to a shared "nice" step so both ends are
    /// round numbers and the four gridlines split evenly into round numbers
    /// too. Only the visible span counts, not the padding kept for warm-up.
    ///
    /// An exact instantaneous max would still let the line poke above the
    /// top line the moment the peak sample's own smoothing nudges it a touch
    /// higher on the next frame; rounding up leaves headroom. And because the
    /// bounds only move when the visible range crosses into a different nice
    /// step, they change far less often than the raw max would.
    fn y_bounds(&self) -> (f64, f64) {
        let visible = self
            .data
            .iter()
            .filter(|point| point.seconds_ago <= SPEED_HISTORY_SPAN.as_secs_f64())
            .flat_map(|point| {
                let disk = Some(point.disk_bytes_per_sec);
                let download = self.show_download.then_some(point.download_bytes_per_sec);
                [disk, download]
            })
            .flatten();

        let (mut raw_min, mut raw_max) = (f64::MAX, 0.0_f64);
        for value in visible {
            raw_min = raw_min.min(value);
            raw_max = raw_max.max(value);
        }
        if raw_min > raw_max {
            (raw_min, raw_max) = (0.0, 1.0);
        }

        let unit = axis_unit_bytes(raw_max);
        let range = ((raw_max - raw_min) / unit).max(raw_max / unit * 0.05).max(0.1);
        let step = nice_step(range / 3.0) * unit;

        let bottom = (raw_min / step).floor() * step;
        let top = ((raw_max / step).ceil() * step).max(bottom + 3.0 * step);
        (bottom, top)
    }

    fn pointer_spring(cx: &App) -> Spring {
        Spring::new(cx.theme().motion_tokens().duration_fast).with_epsilon(0.1)
    }
}

impl Plot for SpeedChart {
    fn paint(&mut self, bounds: Bounds<Pixels>, window: &mut Window, cx: &mut App) {
        let (bottom, top) = self.y_bounds();
        let (x, y) = self.scales(bounds, (bottom, top));
        let height = bounds.size.height.as_f32();

        // Four reference lines (bottom, +1/3, +2/3 and the top of the
        // current nice-rounded scale), each labelled with its value in the
        // left gutter.
        let span = top - bottom;
        let grid_values = [
            bottom,
            bottom + span / 3.0,
            bottom + span * 2.0 / 3.0,
            top,
        ];
        let grid_heights: Vec<f32> = grid_values.iter().filter_map(|value| y.tick(value)).collect();

        let plot_bounds = Bounds::new(
            point(bounds.origin.x + px(Y_AXIS_GUTTER), bounds.origin.y),
            size(bounds.size.width - px(Y_AXIS_GUTTER), bounds.size.height),
        );
        Grid::new()
            .y(grid_heights.iter().map(|grid_height| px(*grid_height)).collect::<Vec<_>>())
            .stroke(cx.theme().colors.border.opacity(0.5))
            .dash_array(&[px(4.), px(2.)])
            .paint(&plot_bounds, window);

        let label_color = cx.theme().colors.muted_foreground;
        PlotLabel::from(grid_values.iter().zip(grid_heights.iter()).map(
            |(value, grid_height)| {
                let label_y = (grid_height - TEXT_SIZE / 2.0).clamp(0., height - TEXT_SIZE);
                Text::new(
                    format_speed(*value),
                    point(px(Y_AXIS_GUTTER - TEXT_GAP * 2.0), px(label_y)),
                    label_color,
                )
                .align(TextAlign::Right)
            },
        ))
        .paint(&bounds, window, cx);

        let (x_for_download, x_for_disk) = (x.clone(), x.clone());
        let (y_for_download, y_for_disk) = (y.clone(), y.clone());

        // Linear, not the default Catmull-Rom spline: a spline overshoots past
        // sharp jumps (e.g. a fresh sample after a phase reset), which for a
        // value that can never be negative reads as the line dipping below
        // zero. Straight segments through the already-smoothed points stay
        // within the data's own range.
        let disk_line = Line::new()
            .data(self.data.iter().copied())
            .x(move |point: &SpeedPoint| x_for_disk.tick(&point.seconds_ago))
            .y(move |point: &SpeedPoint| y_for_disk.tick(&point.disk_bytes_per_sec))
            .stroke(self.disk_stroke)
            .stroke_width(px(2.))
            .stroke_style(StrokeStyle::Linear);
        let download_line = self.show_download.then(|| {
            Line::new()
                .data(self.data.iter().copied())
                .x(move |point: &SpeedPoint| x_for_download.tick(&point.seconds_ago))
                .y(move |point: &SpeedPoint| y_for_download.tick(&point.download_bytes_per_sec))
                .stroke(self.download_stroke)
                .stroke_width(px(2.))
                .stroke_style(StrokeStyle::Linear)
        });

        // The data extends slightly past the visible left edge (see
        // `HISTORY_PADDING`); clip it there so it reads as padding, not as a
        // line poking into the y-axis labels.
        window.with_content_mask(Some(ContentMask { bounds: plot_bounds }), |window| {
            if self.id.is_some() {
                let caches = PathCaches::for_paint("speed-chart-lines", window, cx);
                caches.update(cx, |caches, _| {
                    disk_line.paint_cached(&bounds, caches.slot(0), window);
                    if let Some(download_line) = &download_line {
                        download_line.paint_cached(&bounds, caches.slot(1), window);
                    }
                });
            } else {
                disk_line.paint(&bounds, window);
                if let Some(download_line) = &download_line {
                    download_line.paint(&bounds, window);
                }
            }
        });
    }

    fn id(&self) -> Option<ElementId> {
        self.id.clone()
    }

    fn tooltip_state(
        &self,
        position: Point<Pixels>,
        bounds: Bounds<Pixels>,
        _cx: &App,
    ) -> Option<TooltipState> {
        if self.data.is_empty() || position.x.as_f32() < Y_AXIS_GUTTER {
            return None;
        }
        let (x, y) = self.scales(bounds, self.y_bounds());
        let domain: Vec<f64> = self.data.iter().map(|point| point.seconds_ago).collect();
        let (index, x_tick) = x.least_index_with_domain(position.x.as_f32(), &domain);
        let point_data = self.data.get(index)?;
        let disk_tick = y.tick(&point_data.disk_bytes_per_sec)?;

        let mut dots = vec![point(px(x_tick), px(disk_tick))];
        if self.show_download {
            let download_tick = y.tick(&point_data.download_bytes_per_sec)?;
            dots.push(point(px(x_tick), px(download_tick)));
        }

        Some(TooltipState::new(
            index,
            point(px(x_tick), position.y),
            dots,
        ))
    }

    fn hover(&mut self, hover: Option<&PlotHover>, window: &mut Window, cx: &mut App) {
        self.hover = hover.and_then(|hover| {
            let dots = &hover.state().dots;
            let disk_target = *dots.first()?;
            let download_target = self.show_download.then(|| dots.get(1)).flatten().copied();
            let policy = Self::pointer_spring(cx).with_travel(!hover.is_entering());
            let disk_dot = point(
                spring(("speed-chart", "disk-x"), disk_target.x, policy, window, cx),
                spring(("speed-chart", "disk-y"), disk_target.y, policy, window, cx),
            );
            let download_dot = download_target.map(|target| {
                point(
                    spring(("speed-chart", "download-x"), target.x, policy, window, cx),
                    spring(("speed-chart", "download-y"), target.y, policy, window, cx),
                )
            });
            Some(ChartHover {
                download_dot,
                disk_dot,
                focus: hover.focus(),
            })
        });
    }

    fn tooltip(
        &self,
        state: &TooltipState,
        cursor: Point<Pixels>,
        bounds: Bounds<Pixels>,
        _window: &mut Window,
        cx: &mut App,
    ) -> Option<AnyElement> {
        let point_data = self.data.get(state.index)?;
        let (download_dot, disk_dot, focus) = match self.hover {
            Some(hover) => (hover.download_dot, hover.disk_dot, hover.focus),
            None => (
                self.show_download.then(|| state.dots.get(1)).flatten().copied(),
                *state.dots.first()?,
                1.,
            ),
        };

        let mut dots = vec![
            Dot::new(disk_dot)
                .size(HOVER_DOT_SIZE)
                .halo(px(HOVER_HALO_SIZE * focus))
                .stroke(cx.theme().colors.background)
                .fill(self.disk_stroke),
        ];
        let mut tooltip = Tooltip::new(cursor, bounds.size)
            .gap(px(8.))
            .cross_line(
                CrossLine::new(point(disk_dot.x, state.cross_line.y))
                    .height(bounds.size.height.as_f32()),
            );
        if let Some(download_dot) = download_dot {
            dots.push(
                Dot::new(download_dot)
                    .size(HOVER_DOT_SIZE)
                    .halo(px(HOVER_HALO_SIZE * focus))
                    .stroke(cx.theme().colors.background)
                    .fill(self.download_stroke),
            );
            tooltip = tooltip.row(
                self.download_stroke,
                "Download",
                format_speed(point_data.download_bytes_per_sec),
            );
        }
        tooltip = tooltip.row(
            self.disk_stroke,
            if self.show_download { "Disk" } else { "Validate" },
            format_speed(point_data.disk_bytes_per_sec),
        );

        Some(tooltip.dots(dots).into_any_element())
    }
}
