//! Extra numbers computed alongside a timing: how many bytes a serializer
//! wrote, how well its output compresses, how much memory it peaked at.
//!
//! A [`Metrics`] is a record of named numbers about one cell - one candidate
//! on one input. A group's [`MetricColumn`]s hold the same numbers laid out
//! like its measurements, one per candidate and input, so a table can show
//! them beside the time.
//!
//! Each number is exact rather than sampled, so it has no error bar and
//! nothing here asks whether a difference is significant.

/// What a metric is counted in, which is all that decides how it is
/// printed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Unit {
    /// Bytes, printed in the largest binary unit that keeps it above one:
    /// `812B`, `1.90MiB`.
    Bytes,
    /// A plain count, printed whole when it is whole.
    Count,
    /// A dimensionless number, such as a compression ratio.
    Ratio,
    /// A percentage, printed with a `%`.
    Percent,
    /// A duration in seconds, printed in the unit that suits it, as a
    /// timing is.
    Seconds,
    /// Anything else: the text is printed straight after the number, so
    /// `"ops/s"` gives `1.23ops/s` and `" ops/s"` gives `1.23 ops/s`.
    Custom(&'static str),
}

impl Unit {
    /// `value` as this unit prints it.
    ///
    /// Three significant digits, which is as much as anyone reads off a
    /// table; a metric that needs more can be a [`Unit::Custom`] on a
    /// rescaled value.
    pub fn format(self, value: f64) -> String {
        if !value.is_finite() {
            return "-".to_string();
        }
        match self {
            Unit::Bytes => {
                const UNITS: [&str; 6] = ["B", "KiB", "MiB", "GiB", "TiB", "PiB"];
                let mut scaled = value;
                let mut unit = 0;
                while scaled.abs() >= 1024.0 && unit + 1 < UNITS.len() {
                    scaled /= 1024.0;
                    unit += 1;
                }
                if unit == 0 && scaled.fract() == 0.0 {
                    format!("{scaled}B")
                } else {
                    format!("{}{}", three_digits(scaled), UNITS[unit])
                }
            }
            Unit::Count => {
                if value.fract() == 0.0 && value.abs() < 1e15 {
                    format!("{value}")
                } else {
                    three_digits(value)
                }
            }
            Unit::Ratio => three_digits(value),
            Unit::Percent => format!("{}%", three_digits(value)),
            Unit::Seconds => {
                let (divisor, unit) = crate::unit_for(value * 1e9);
                format!("{}{unit}", three_digits(value * 1e9 / divisor))
            }
            Unit::Custom(suffix) => format!("{}{suffix}", three_digits(value)),
        }
    }
}

/// `x` to three significant digits, without a trailing exponent.
fn three_digits(x: f64) -> String {
    if x == 0.0 {
        return "0".to_string();
    }
    let decimals = (2 - x.abs().log10().floor() as i64).clamp(0, 9) as usize;
    format!("{x:.decimals$}")
}

/// One named number about a cell.
#[derive(Debug, Clone, PartialEq)]
pub struct Metric {
    /// What the column is called.
    pub name: String,
    /// The value, in `unit`.
    pub value: f64,
    /// What it is counted in.
    pub unit: Unit,
}

/// What a value can be turned into a metric from: any number, or an
/// `Option` of one, where `None` leaves the metric out.
pub trait IntoMetric {
    /// The value as an `f64`, or `None` to say there is none.
    fn into_metric(self) -> Option<f64>;
}

macro_rules! numeric_metrics {
    ($($t:ty),*) => {$(
        impl IntoMetric for $t {
            fn into_metric(self) -> Option<f64> {
                Some(self as f64)
            }
        }
    )*};
}
numeric_metrics!(f64, f32, u8, u16, u32, u64, u128, usize, i8, i16, i32, i64, i128, isize);

impl<T: IntoMetric> IntoMetric for Option<T> {
    fn into_metric(self) -> Option<f64> {
        self.and_then(IntoMetric::into_metric)
    }
}

/// A record of named numbers about one cell, built a field at a time.
///
/// ```
/// use scaling::{Metrics, Unit};
///
/// let out = vec![0u8; 2048];
/// let metrics = Metrics::new()
///     .bytes("size", out.len())
///     .ratio("per item", out.len() as f64 / 16.0)
///     .value("throughput", 1.5e6)
///     .unit(Unit::Custom(" ops/s"));
/// assert_eq!(metrics.get("size").unwrap().value, 2048.0);
/// assert_eq!(metrics.iter().count(), 3);
/// ```
///
/// A name given twice keeps the later value, in the earlier position. A value
/// of `None` leaves the metric out, so it shows as missing in the table.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Metrics {
    metrics: Vec<Metric>,
    /// Metrics whose values are not known until the run has been counted:
    /// the index of each, in `metrics`, and which count it is.
    counted: Vec<(usize, Counted)>,
}

/// Which of an alternative's [`AllocStats`] a metric shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Counted {
    PeakBytes,
    Allocations,
    AllocatedBytes,
    RetainedBytes,
}

impl Metrics {
    /// An empty record.
    pub fn new() -> Self {
        Metrics::default()
    }

    fn push(mut self, name: &str, value: impl IntoMetric, unit: Unit) -> Self {
        let Some(value) = value.into_metric() else {
            return self;
        };
        let metric = Metric {
            name: name.to_string(),
            value,
            unit,
        };
        match self.metrics.iter().position(|m| m.name == name) {
            Some(at) => {
                self.metrics[at] = metric;
                // A value given outright replaces one that was to be counted.
                self.counted.retain(|(i, _)| *i != at);
            }
            None => self.metrics.push(metric),
        }
        self
    }

    /// A metric to be filled in from the counted run, once there is one.
    fn counted(mut self, name: &str, unit: Unit, which: Counted) -> Self {
        self = self.push(name, f64::NAN, unit);
        let at = self
            .metrics
            .iter()
            .position(|m| m.name == name)
            .expect("just added");
        self.counted.retain(|(i, _)| *i != at);
        self.counted.push((at, which));
        self
    }

    /// A size in bytes.
    pub fn bytes(self, name: &str, value: impl IntoMetric) -> Self {
        self.push(name, value, Unit::Bytes)
    }

    /// A count.
    pub fn count(self, name: &str, value: impl IntoMetric) -> Self {
        self.push(name, value, Unit::Count)
    }

    /// A dimensionless number.
    pub fn ratio(self, name: &str, value: impl IntoMetric) -> Self {
        self.push(name, value, Unit::Ratio)
    }

    /// A percentage.
    pub fn percent(self, name: &str, value: impl IntoMetric) -> Self {
        self.push(name, value, Unit::Percent)
    }

    /// A duration in seconds.
    pub fn seconds(self, name: &str, value: impl IntoMetric) -> Self {
        self.push(name, value, Unit::Seconds)
    }

    /// A number in no particular unit, to be given one with
    /// [`Metrics::unit`].
    pub fn value(self, name: &str, value: impl IntoMetric) -> Self {
        self.push(name, value, Unit::Custom(""))
    }

    /// The most memory the candidate held at once, as a `peak` metric.
    ///
    /// Needs `allocation` in the `#[scaling::metrics(..)]` of the function
    /// that builds this, and [`CountingAlloc`](crate::alloc::CountingAlloc)
    /// as the global allocator. Memory the candidate was handed, such as its
    /// input, is not counted: only what it allocated itself.
    pub fn peak_bytes(self) -> Self {
        self.counted("peak", Unit::Bytes, Counted::PeakBytes)
    }

    /// How many times the candidate asked for memory, as an `allocs`
    /// metric. See [`Metrics::peak_bytes`] for what that needs.
    pub fn allocations(self) -> Self {
        self.counted("allocs", Unit::Count, Counted::Allocations)
    }

    /// How much memory the candidate asked for in all, as an `allocated`
    /// metric: what it asked for again each time it did, not what it held at
    /// the most. See [`Metrics::peak_bytes`] for what that needs.
    pub fn allocated_bytes(self) -> Self {
        self.counted("allocated", Unit::Bytes, Counted::AllocatedBytes)
    }

    /// How much more memory the candidate held when its call ended than when
    /// it began, as a `retained` metric: what it returned, and anything else
    /// it kept. Negative if it freed memory it was handed. See
    /// [`Metrics::peak_bytes`] for what that needs.
    pub fn retained_bytes(self) -> Self {
        self.counted("retained", Unit::Bytes, Counted::RetainedBytes)
    }

    /// The counts of the run this record is being built for, so that a metric
    /// can be computed from them.
    ///
    /// ```ignore
    /// #[scaling::metrics(group = "encode", allocation)]
    /// fn overhead(out: Vec<u8>) -> scaling::Metrics {
    ///     let held = scaling::Metrics::allocation_counts().map_or(0, |c| c.retained_bytes);
    ///     scaling::Metrics::new().ratio("held per byte", held as f64 / out.len() as f64)
    /// }
    /// ```
    ///
    /// `None` anywhere else, and in a metrics function that is not marked
    /// `allocation`, since its run was not counted. The counts are those of
    /// the candidate's own call, fixed before the function started, so
    /// whatever the function allocates does not change them.
    pub fn allocation_counts() -> Option<crate::alloc::AllocStats> {
        crate::alloc::current()
    }

    /// Whether any of the metrics wait on a counted run.
    #[cfg(test)]
    pub(crate) fn wants_allocation(&self) -> bool {
        !self.counted.is_empty()
    }

    /// Fill in the metrics that wait on a counted run.
    ///
    /// # Panics
    ///
    /// If there are some, and `stats` is `None`: the run was not counted,
    /// so what they would show is nothing at all.
    pub(crate) fn resolve_allocation(&mut self, stats: Option<crate::alloc::AllocStats>) {
        if self.counted.is_empty() {
            return;
        }
        let stats = stats.expect(
            "a metrics function asks for allocation numbers (`peak_bytes`, `allocations` or \
             `allocated_bytes`), but the run was not counted - say `allocation` in its \
             #[scaling::metrics(..)]",
        );
        for (at, which) in self.counted.drain(..) {
            self.metrics[at].value = match which {
                Counted::PeakBytes => stats.peak_bytes as f64,
                Counted::Allocations => stats.allocations as f64,
                Counted::AllocatedBytes => stats.allocated_bytes as f64,
                Counted::RetainedBytes => stats.retained_bytes as f64,
            };
        }
    }

    /// Changes the unit of the metric added last. Does nothing on an empty
    /// record.
    pub fn unit(mut self, unit: Unit) -> Self {
        if let Some(last) = self.metrics.last_mut() {
            last.unit = unit;
        }
        self
    }

    /// Every metric, in the order they were added.
    pub fn iter(&self) -> impl Iterator<Item = &Metric> {
        self.metrics.iter()
    }

    /// The metric of this name, if there is one.
    pub fn get(&self, name: &str) -> Option<&Metric> {
        self.metrics.iter().find(|m| m.name == name)
    }

    /// Whether there are no metrics.
    pub fn is_empty(&self) -> bool {
        self.metrics.is_empty()
    }
}

/// One metric across a whole [`Group`](crate::Group), shaped like its
/// measurements.
#[derive(Debug, Clone, PartialEq)]
pub struct MetricColumn {
    /// What the metric is called.
    pub name: String,
    /// What it is counted in.
    pub unit: Unit,
    /// `values[candidate][input]`, aligned with the group's measurements.
    /// `None` where that cell produced no such metric.
    pub values: Vec<Vec<Option<f64>>>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bytes_pick_the_unit_that_keeps_them_above_one() {
        assert_eq!(Unit::Bytes.format(0.0), "0B");
        assert_eq!(Unit::Bytes.format(812.0), "812B");
        assert_eq!(Unit::Bytes.format(1023.0), "1023B");
        assert_eq!(Unit::Bytes.format(1024.0), "1.00KiB");
        assert_eq!(Unit::Bytes.format(612.0 * 1024.0), "612KiB");
        assert_eq!(Unit::Bytes.format(1.9 * 1024.0 * 1024.0), "1.90MiB");
        assert_eq!(Unit::Bytes.format(37.5 * 1024.0 * 1024.0), "37.5MiB");
        assert_eq!(Unit::Bytes.format(1.5), "1.50B");
    }

    #[test]
    fn other_units_print_three_digits() {
        assert_eq!(Unit::Count.format(12.0), "12");
        assert_eq!(Unit::Count.format(1_234_567.0), "1234567");
        assert_eq!(Unit::Count.format(0.5), "0.500");
        assert_eq!(Unit::Ratio.format(0.31234), "0.312");
        assert_eq!(Unit::Ratio.format(41.23), "41.2");
        assert_eq!(Unit::Percent.format(12.345), "12.3%");
        assert_eq!(Unit::Seconds.format(0.0125), "12.5ms");
        assert_eq!(Unit::Seconds.format(2.0), "2.00s");
        assert_eq!(Unit::Custom("ops/s").format(1234.5), "1234ops/s");
        assert_eq!(Unit::Custom(" ops/s").format(1.5), "1.50 ops/s");
    }

    #[test]
    fn a_value_that_is_not_a_number_prints_as_missing() {
        for unit in [Unit::Bytes, Unit::Count, Unit::Ratio, Unit::Seconds] {
            assert_eq!(unit.format(f64::NAN), "-");
            assert_eq!(unit.format(f64::INFINITY), "-");
        }
    }

    #[test]
    fn a_name_given_twice_keeps_the_later_value_in_the_earlier_place() {
        let m = Metrics::new()
            .bytes("a", 1usize)
            .bytes("b", 2usize)
            .bytes("a", 3usize);
        let values: Vec<_> = m.iter().map(|m| (m.name.as_str(), m.value)).collect();
        assert_eq!(values, [("a", 3.0), ("b", 2.0)]);
    }

    #[test]
    fn none_leaves_the_metric_out() {
        let m = Metrics::new()
            .bytes("size", None::<usize>)
            .bytes("x", Some(4u8));
        assert!(m.get("size").is_none());
        assert_eq!(m.get("x").unwrap().value, 4.0);
    }

    #[test]
    fn unit_changes_the_last_metric_only() {
        let m = Metrics::new()
            .value("first", 1.0)
            .value("rate", 2.0)
            .unit(Unit::Custom("/s"));
        assert_eq!(m.get("first").unwrap().unit, Unit::Custom(""));
        assert_eq!(m.get("rate").unwrap().unit, Unit::Custom("/s"));
        // On an empty record there is nothing to change.
        assert!(Metrics::new().unit(Unit::Bytes).is_empty());
    }

    #[test]
    fn counted_metrics_wait_for_the_run_and_keep_their_place() {
        let mut m = Metrics::new()
            .bytes("size", 10usize)
            .peak_bytes()
            .count("items", 3u32)
            .allocations()
            .allocated_bytes()
            .retained_bytes();
        assert!(m.wants_allocation());
        m.resolve_allocation(Some(crate::alloc::AllocStats {
            peak_bytes: 400,
            allocations: 7,
            allocated_bytes: 900,
            retained_bytes: -120,
        }));
        assert!(!m.wants_allocation());
        let got: Vec<_> = m
            .iter()
            .map(|m| (m.name.as_str(), m.value, m.unit))
            .collect();
        assert_eq!(
            got,
            [
                ("size", 10.0, Unit::Bytes),
                ("peak", 400.0, Unit::Bytes),
                ("items", 3.0, Unit::Count),
                ("allocs", 7.0, Unit::Count),
                ("allocated", 900.0, Unit::Bytes),
                ("retained", -120.0, Unit::Bytes),
            ]
        );
    }

    #[test]
    fn a_value_given_outright_replaces_a_counted_one() {
        let mut m = Metrics::new().peak_bytes().bytes("peak", 5usize);
        assert!(!m.wants_allocation());
        m.resolve_allocation(None);
        assert_eq!(m.get("peak").unwrap().value, 5.0);
    }

    #[test]
    fn nothing_to_resolve_needs_no_counts() {
        let mut m = Metrics::new().bytes("size", 1usize);
        m.resolve_allocation(None);
        assert_eq!(m.get("size").unwrap().value, 1.0);
    }

    #[test]
    #[should_panic(expected = "say `allocation`")]
    fn asking_for_counts_from_an_uncounted_run_is_an_error() {
        Metrics::new().peak_bytes().resolve_allocation(None);
    }

    #[test]
    fn allocation_counts_are_available_to_a_metrics_function_while_it_runs() {
        assert_eq!(Metrics::allocation_counts(), None);
        let stats = crate::alloc::AllocStats {
            retained_bytes: 42,
            ..Default::default()
        };
        let provided = crate::alloc::provide(Some(stats));
        assert_eq!(Metrics::allocation_counts(), Some(stats));
        drop(provided);
        assert_eq!(Metrics::allocation_counts(), None);
    }
}
