//! Extra numbers computed alongside a timing: how many bytes a serializer
//! wrote, how well its output compresses, how much memory it peaked at.
//!
//! A [`Metrics`] is a record of named [`MetricValue`]s about one cell - one
//! candidate on one input. A group's [`MetricColumn`]s hold the same numbers
//! laid out like its measurements, one per candidate and input, so a table can
//! show them beside the time. Neither the columns nor the group is exposed: a
//! script reads the numbers by name through `Timings::metrics`.
//!
//! Each number is exact rather than sampled, so it has no error bar and
//! nothing here asks whether a difference is significant.

use std::fmt::{self, Write as _};
use std::time::Duration;

/// A number and what it is counted in, which together decide how it is
/// printed.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Value {
    /// A size, printed in the largest binary unit that keeps it above one.
    /// Held as a float so that a mean size need not be rounded.
    Bytes(f64),
    /// A whole number: a count, or a signed difference of counts.
    Integer(i128),
    /// A dimensionless number, such as a ratio.
    Float(f64),
    /// A percentage.
    Percent(f64),
    /// A duration.
    Time(Duration),
}

/// One number about a cell, together with what it is counted in.
///
/// Numeric types convert to one - integers to whole numbers, floats to
/// plain numbers, a [`Duration`] to a time - and the constructors make the
/// others. It prints to three significant digits in the unit that suits it
/// (`1.90MiB`, `12.5ms`, `0.312`), or to a fixed number of decimals when asked:
///
/// ```
/// use scaling::MetricValue;
/// use std::time::Duration;
///
/// assert_eq!(MetricValue::bytes(2048usize).to_string(), "2.00KiB");
/// assert_eq!(MetricValue::from(12).to_string(), "12");
/// assert_eq!(MetricValue::from(0.31234).to_string(), "0.312");
/// assert_eq!(MetricValue::percent(12.345).to_string(), "12.3%");
/// assert_eq!(MetricValue::from(Duration::from_micros(12500)).to_string(), "12.5ms");
///
/// // Width, fill, alignment and precision are honoured.
/// assert_eq!(format!("{:.1}", MetricValue::bytes(1992294)), "1.9MiB");
/// assert_eq!(format!("{:>8}", MetricValue::bytes(812)), "    812B");
/// ```
///
/// A value that is not a finite number prints as `-`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MetricValue(Value);

impl MetricValue {
    /// A size in bytes. A float is kept as it is, so a mean size is not
    /// rounded; whole sizes print as `812B`, and larger ones as `1.90MiB`.
    pub fn bytes(size: impl Into<MetricValue>) -> Self {
        MetricValue(Value::Bytes(size.into().as_f64()))
    }

    /// A whole number, such as a count. A float is rounded to the nearest
    /// whole number.
    pub fn count(n: impl Into<MetricValue>) -> Self {
        match n.into().0 {
            Value::Integer(i) => MetricValue(Value::Integer(i)),
            other => {
                let x = MetricValue(other).as_f64();
                if x.is_finite() {
                    MetricValue(Value::Integer(x.round() as i128))
                } else {
                    MetricValue(Value::Float(x))
                }
            }
        }
    }

    /// A dimensionless number, such as a ratio, printed to three significant
    /// digits even when it happens to be whole.
    pub fn ratio(x: impl Into<MetricValue>) -> Self {
        MetricValue(Value::Float(x.into().as_f64()))
    }

    /// A percentage, so `12.3` prints as `12.3%`.
    pub fn percent(x: impl Into<MetricValue>) -> Self {
        MetricValue(Value::Percent(x.into().as_f64()))
    }

    /// A duration, printed in the unit that suits it, as a timing is.
    pub fn time(d: Duration) -> Self {
        MetricValue(Value::Time(d))
    }

    /// The number, in bytes, whole units, or seconds, as the kind it is; the
    /// way to compare values or read one back.
    pub fn as_f64(&self) -> f64 {
        match self.0 {
            Value::Bytes(x) | Value::Float(x) | Value::Percent(x) => x,
            Value::Integer(i) => i as f64,
            Value::Time(d) => d.as_secs_f64(),
        }
    }

    /// Whether `other` is counted in the same unit, which is what a
    /// difference between the two needs.
    pub(crate) fn same_kind(&self, other: &MetricValue) -> bool {
        std::mem::discriminant(&self.0) == std::mem::discriminant(&other.0)
    }
}

macro_rules! whole_numbers {
    ($($t:ty),*) => {$(
        impl From<$t> for MetricValue {
            fn from(n: $t) -> Self {
                MetricValue(Value::Integer(n as i128))
            }
        }
    )*};
}
whole_numbers!(u8, u16, u32, u64, usize, i8, i16, i32, i64, isize, i128);

macro_rules! floating_numbers {
    ($($t:ty),*) => {$(
        impl From<$t> for MetricValue {
            fn from(x: $t) -> Self {
                MetricValue(Value::Float(x as f64))
            }
        }
    )*};
}
floating_numbers!(f32, f64);

impl From<Duration> for MetricValue {
    fn from(d: Duration) -> Self {
        MetricValue(Value::Time(d))
    }
}

impl fmt::Display for MetricValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let precision = f.precision();
        let text = match self.0 {
            Value::Bytes(size) => bytes_text(size, precision),
            Value::Integer(n) => n.to_string(),
            Value::Float(x) => digits(x, precision),
            Value::Percent(x) if x.is_finite() => format!("{}%", digits(x, precision)),
            Value::Percent(_) => "-".to_string(),
            Value::Time(d) => {
                let ns = d.as_secs_f64() * 1e9;
                let (divisor, unit) = crate::unit_for(ns);
                format!("{}{unit}", digits(ns / divisor, precision))
            }
        };
        pad(f, &text)
    }
}

/// `text` with the width, fill and alignment `f` asks for. Not
/// [`Formatter::pad`], which would read a precision as a length to cut the
/// text to.
fn pad(f: &mut fmt::Formatter<'_>, text: &str) -> fmt::Result {
    let Some(width) = f.width() else {
        return f.write_str(text);
    };
    let shown = text.chars().count();
    let Some(gap) = width.checked_sub(shown).filter(|gap| *gap > 0) else {
        return f.write_str(text);
    };
    let (before, after) = match f.align() {
        Some(fmt::Alignment::Left) => (0, gap),
        Some(fmt::Alignment::Center) => (gap / 2, gap - gap / 2),
        // A number reads best against the right edge.
        Some(fmt::Alignment::Right) | None => (gap, 0),
    };
    for _ in 0..before {
        f.write_char(f.fill())?;
    }
    f.write_str(text)?;
    for _ in 0..after {
        f.write_char(f.fill())?;
    }
    Ok(())
}

/// `x` to the decimals asked for, or to three significant digits.
fn digits(x: f64, precision: Option<usize>) -> String {
    if !x.is_finite() {
        return "-".to_string();
    }
    match precision {
        Some(decimals) => format!("{x:.decimals$}"),
        None => three_digits(x),
    }
}

/// `x` to three significant digits, in decimals unless it is too small for nine
/// of them to show three, and then in scientific notation (`1.00e-12`) rather
/// than as a row of zeros.
fn three_digits(x: f64) -> String {
    if x == 0.0 {
        return "0".to_string();
    }
    // Rounding to three significant digits comes first, and the exponent is
    // read from the result, so that a carry into the next power of ten (9.996
    // is 10.0, not 10.00) is already in it.
    let scientific = format!("{x:.2e}");
    let exponent: i32 = scientific
        .rsplit('e')
        .next()
        .and_then(|exponent| exponent.parse().ok())
        .expect("`{:e}` ends in an exponent");
    let decimals = 2 - exponent;
    if decimals > 9 {
        scientific
    } else {
        format!("{x:.*}", decimals.max(0) as usize)
    }
}

fn bytes_text(size: f64, precision: Option<usize>) -> String {
    const UNITS: [&str; 6] = ["B", "KiB", "MiB", "GiB", "TiB", "PiB"];
    if !size.is_finite() {
        return "-".to_string();
    }
    let mut scaled = size;
    let mut unit = 0;
    while scaled.abs() >= 1024.0 && unit + 1 < UNITS.len() {
        scaled /= 1024.0;
        unit += 1;
    }
    if precision.is_none() && unit == 0 && scaled.fract() == 0.0 {
        format!("{scaled}B")
    } else {
        format!("{}{}", digits(scaled, precision), UNITS[unit])
    }
}

/// A record of named numbers about one cell - one candidate on one input -
/// built a field at a time. It is what a [`metrics`](macro@crate::metrics)
/// function returns, and the crate docs for that attribute say what happens to
/// it next.
///
/// ```
/// use scaling::{MetricValue, Metrics};
/// use std::time::Duration;
///
/// let out = vec![0u8; 2048];
/// let metrics = Metrics::new()
///     .bytes("size", out.len())
///     .ratio("per item", out.len() as f64 / 16.0)
///     .add("setup", Duration::from_millis(12));
/// assert_eq!(metrics.get("size"), Some(MetricValue::bytes(2048)));
/// assert_eq!(metrics.iter().count(), 3);
/// ```
///
/// # Adding numbers
///
/// [`add`](Metrics::add) takes a name, which is a column of the table, and
/// anything that converts to a [`MetricValue`]: an integer, a float, a
/// [`Duration`], or a value built with one of its constructors. A bare number
/// carries no unit, so the wrappers say it by name:
///
/// | method | is | printed as |
/// |---|---|---|
/// | [`bytes`](Metrics::bytes) | a size | `812B`, `1.90MiB` |
/// | [`count`](Metrics::count) | a whole number | `12` |
/// | [`ratio`](Metrics::ratio) | a plain number | `0.312` |
/// | [`percent`](Metrics::percent) | a percentage | `12.3%` |
///
/// These read the number they are given, so a [`Duration`] passed to `count` is
/// its seconds, and a [`MetricValue`] loses the unit it had; to keep a time as a
/// time, pass it to [`add`](Metrics::add).
///
/// A metric that is not defined is simply not added, and shows as `-` in the
/// table. A name given twice keeps the later value, in the earlier position.
///
/// # Allocation numbers
///
/// Four more methods stand for numbers that are not known until the candidate
/// has run: [`peak_allocated_bytes`](Metrics::peak_allocated_bytes) (column
/// `alloc peak`), [`allocation_count`](Metrics::allocation_count) (`alloc
/// count`), [`total_allocated_bytes`](Metrics::total_allocated_bytes) (`alloc
/// total`) and [`net_allocated_bytes`](Metrics::net_allocated_bytes) (`alloc
/// net`). They need `allocation` in the attribute and the counting
/// [`Allocator`](crate::Allocator) installed; [`Metrics::allocations`] reads the
/// same numbers, to compute another from them, and
/// [`Metrics::allocator_installed`] says whether the allocator is in use.
///
/// # Reading one back
///
/// [`get`](Metrics::get) gives a value by name and [`iter`](Metrics::iter) every
/// name and value in the order they were added. A script gets one record for
/// each alternative from `Timings::metrics`.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Metrics {
    entries: Vec<(String, Entry)>,
}

/// What a name stands for in a [`Metrics`].
#[derive(Debug, Clone, Copy, PartialEq)]
enum Entry {
    Value(MetricValue),
    /// A number not known until the run has been counted.
    Counted(Counted),
}

/// Which of an alternative's [`Allocations`] a metric shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Counted {
    AllocationCount,
    PeakAllocatedBytes,
    TotalAllocatedBytes,
    NetAllocatedBytes,
}

/// What an alternative that computed nothing has, for a caller that is handed a
/// reference.
pub(crate) static NO_METRICS: Metrics = Metrics::new();

impl Metrics {
    /// An empty record.
    pub const fn new() -> Self {
        Metrics {
            entries: Vec::new(),
        }
    }

    fn put(mut self, name: &str, entry: Entry) -> Self {
        match self.entries.iter_mut().find(|(n, _)| n == name) {
            Some((_, existing)) => *existing = entry,
            None => self.entries.push((name.to_string(), entry)),
        }
        self
    }

    /// Add a number under `name`: an integer, a float, a [`Duration`], or a
    /// [`MetricValue`] built with the unit it is counted in.
    pub fn add(self, name: &str, value: impl Into<MetricValue>) -> Self {
        self.put(name, Entry::Value(value.into()))
    }

    /// A size in bytes. See [`MetricValue::bytes`].
    pub fn bytes(self, name: &str, size: impl Into<MetricValue>) -> Self {
        self.add(name, MetricValue::bytes(size))
    }

    /// A whole number. See [`MetricValue::count`].
    pub fn count(self, name: &str, n: impl Into<MetricValue>) -> Self {
        self.add(name, MetricValue::count(n))
    }

    /// A dimensionless number. See [`MetricValue::ratio`].
    pub fn ratio(self, name: &str, x: impl Into<MetricValue>) -> Self {
        self.add(name, MetricValue::ratio(x))
    }

    /// A percentage. See [`MetricValue::percent`].
    pub fn percent(self, name: &str, x: impl Into<MetricValue>) -> Self {
        self.add(name, MetricValue::percent(x))
    }

    /// How many times the candidate asked for memory, as an `alloc count`
    /// metric.
    ///
    /// Needs `allocation` in the `#[scaling::metrics(..)]` of the function
    /// that builds this, and [`Allocator`](crate::Allocator) as the global
    /// allocator. Only the candidate's own call is counted, so memory it was
    /// handed, such as its input, is not.
    pub fn allocation_count(self) -> Self {
        self.put("alloc count", Entry::Counted(Counted::AllocationCount))
    }

    /// The most memory the candidate held at once, as an `alloc peak` metric.
    /// See [`Metrics::allocation_count`] for what that needs.
    pub fn peak_allocated_bytes(self) -> Self {
        self.put("alloc peak", Entry::Counted(Counted::PeakAllocatedBytes))
    }

    /// How much memory the candidate asked for in all, as an `alloc total`
    /// metric: what it asked for again each time it did, not what it held at the
    /// most. See [`Metrics::allocation_count`] for what that needs.
    pub fn total_allocated_bytes(self) -> Self {
        self.put("alloc total", Entry::Counted(Counted::TotalAllocatedBytes))
    }

    /// How much more memory the candidate held when its call ended than when it
    /// began, as an `alloc net` metric: what it allocated less what it freed.
    /// That is usually what it returned, but also anything it kept some other
    /// way, and it is negative if it freed memory it was handed. See
    /// [`Metrics::allocation_count`] for what that needs.
    pub fn net_allocated_bytes(self) -> Self {
        self.put("alloc net", Entry::Counted(Counted::NetAllocatedBytes))
    }

    /// What the candidate allocated, for a metric computed from it.
    ///
    /// ```ignore
    /// #[scaling::metrics(group = "encode", allocation)]
    /// fn overhead(out: Vec<u8>) -> scaling::Metrics {
    ///     let held = scaling::Metrics::allocations().map_or(0, |a| a.net_allocated_bytes);
    ///     scaling::Metrics::new().ratio("held per byte", held as f64 / out.len() as f64)
    /// }
    /// ```
    ///
    /// `None` anywhere else, and in a metrics function that is not marked
    /// `allocation`, since its run was not counted. The counts are those of the
    /// candidate's own call, fixed before the function started, so whatever the
    /// function allocates does not change them.
    pub fn allocations() -> Option<crate::alloc::Allocations> {
        crate::alloc::current()
    }

    /// Whether [`Allocator`](crate::Allocator) is the global allocator, which
    /// the allocation metrics need. Only meaningful once the program has
    /// allocated, which anything that has reached `main` has.
    pub fn allocator_installed() -> bool {
        crate::alloc::installed()
    }

    /// Whether any of the metrics wait on a counted run.
    #[cfg(test)]
    pub(crate) fn wants_allocation(&self) -> bool {
        self.entries
            .iter()
            .any(|(_, entry)| matches!(entry, Entry::Counted(_)))
    }

    /// Fill in the metrics that wait on a counted run.
    ///
    /// # Panics
    ///
    /// If there are some, and `stats` is `None`: the run was not counted,
    /// so what they would show is nothing at all.
    pub(crate) fn resolve_allocation(&mut self, stats: Option<crate::alloc::Allocations>) {
        for (_, entry) in &mut self.entries {
            let Entry::Counted(which) = *entry else {
                continue;
            };
            let stats = stats.expect(
                "a metrics function asks for allocation numbers (`allocation_count`, \
                 `peak_allocated_bytes`, `total_allocated_bytes` or `net_allocated_bytes`), but \
                 the run was not counted - say `allocation` in its #[scaling::metrics(..)]",
            );
            *entry = Entry::Value(match which {
                Counted::AllocationCount => MetricValue::from(stats.allocation_count),
                Counted::PeakAllocatedBytes => MetricValue::bytes(stats.peak_allocated_bytes),
                Counted::TotalAllocatedBytes => MetricValue::bytes(stats.total_allocated_bytes),
                Counted::NetAllocatedBytes => MetricValue::bytes(stats.net_allocated_bytes),
            });
        }
    }

    /// Every metric as its name and value, in the order they were added.
    pub fn iter(&self) -> impl Iterator<Item = (&str, MetricValue)> {
        self.entries.iter().filter_map(|(name, entry)| match entry {
            Entry::Value(value) => Some((name.as_str(), *value)),
            Entry::Counted(_) => None,
        })
    }

    /// The metric of this name, if there is one.
    pub fn get(&self, name: &str) -> Option<MetricValue> {
        self.iter()
            .find(|(n, _)| *n == name)
            .map(|(_, value)| value)
    }

    /// Whether there are no metrics.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

/// One metric across a whole [`Group`](crate::Group), shaped like its
/// measurements.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct MetricColumn {
    /// What the metric is called.
    pub(crate) name: String,
    /// `values[candidate][input]`, aligned with the group's measurements.
    /// `None` where that cell produced no such metric.
    pub(crate) values: Vec<Vec<Option<MetricValue>>>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shown(value: impl Into<MetricValue>) -> String {
        value.into().to_string()
    }

    #[test]
    fn bytes_pick_the_unit_that_keeps_them_above_one() {
        assert_eq!(shown(MetricValue::bytes(0)), "0B");
        assert_eq!(shown(MetricValue::bytes(812)), "812B");
        assert_eq!(shown(MetricValue::bytes(1023)), "1023B");
        assert_eq!(shown(MetricValue::bytes(1024)), "1.00KiB");
        assert_eq!(shown(MetricValue::bytes(612 * 1024)), "612KiB");
        assert_eq!(shown(MetricValue::bytes(1.9 * 1024.0 * 1024.0)), "1.90MiB");
        assert_eq!(shown(MetricValue::bytes(37.5 * 1024.0 * 1024.0)), "37.5MiB");
        assert_eq!(shown(MetricValue::bytes(1.5)), "1.50B");
        assert_eq!(shown(MetricValue::bytes(-1536)), "-1.50KiB");
    }

    #[test]
    fn the_other_kinds_print_three_digits() {
        assert_eq!(shown(12), "12");
        assert_eq!(shown(1_234_567u64), "1234567");
        assert_eq!(shown(-4), "-4");
        assert_eq!(shown(0.5), "0.500");
        assert_eq!(shown(0.31234), "0.312");
        assert_eq!(shown(41.23), "41.2");
        assert_eq!(shown(MetricValue::ratio(2)), "2.00");
        assert_eq!(shown(MetricValue::percent(12.345)), "12.3%");
        assert_eq!(shown(Duration::from_micros(12500)), "12.5ms");
        assert_eq!(shown(Duration::from_secs(2)), "2.00s");
        assert_eq!(shown(Duration::ZERO), "0ns");
    }

    #[test]
    fn a_value_that_is_not_a_number_prints_as_missing() {
        for value in [
            MetricValue::bytes(f64::NAN),
            MetricValue::ratio(f64::INFINITY),
            MetricValue::percent(f64::NAN),
            MetricValue::from(f64::NEG_INFINITY),
        ] {
            assert_eq!(value.to_string(), "-");
        }
    }

    #[test]
    fn a_precision_means_decimals_after_scaling() {
        assert_eq!(format!("{:.1}", MetricValue::bytes(1992294)), "1.9MiB");
        assert_eq!(format!("{:.3}", MetricValue::bytes(812)), "812.000B");
        assert_eq!(format!("{:.0}", MetricValue::from(2.6)), "3");
        assert_eq!(format!("{:.2}", MetricValue::percent(12.345)), "12.35%");
        assert_eq!(
            format!("{:.1}", MetricValue::from(Duration::from_micros(12500))),
            "12.5ms"
        );
        // A whole number has no decimals to give.
        assert_eq!(format!("{:.3}", MetricValue::from(7)), "7");
    }

    #[test]
    fn width_fill_and_alignment_are_honoured() {
        let v = MetricValue::bytes(812);
        assert_eq!(format!("{v:>8}"), "    812B");
        assert_eq!(format!("{v:<8}|"), "812B    |");
        assert_eq!(format!("{v:^8}|"), "  812B  |");
        assert_eq!(format!("{v:*>8}"), "****812B");
        assert_eq!(format!("{v:8}"), "    812B");
        assert_eq!(format!("{v:2}"), "812B");
        assert_eq!(format!("{:>9.1}", MetricValue::bytes(1992294)), "   1.9MiB");
    }

    #[test]
    fn numbers_convert_to_the_kind_that_suits_them() {
        assert_eq!(MetricValue::from(3usize), MetricValue::count(3));
        assert_eq!(MetricValue::from(3u8), MetricValue::from(3i64));
        assert_eq!(MetricValue::from(0.5f32), MetricValue::ratio(0.5));
        assert_ne!(MetricValue::from(2), MetricValue::ratio(2));
        assert_eq!(MetricValue::count(2.6), MetricValue::from(3));
        assert_eq!(
            MetricValue::time(Duration::from_secs(1)),
            Duration::from_secs(1).into()
        );
        assert_eq!(MetricValue::bytes(3).as_f64(), 3.0);
        assert_eq!(MetricValue::time(Duration::from_millis(1500)).as_f64(), 1.5);
        assert!(MetricValue::bytes(1).same_kind(&MetricValue::bytes(2.5)));
        assert!(!MetricValue::bytes(1).same_kind(&MetricValue::from(1)));
    }

    #[test]
    fn the_wrappers_say_what_a_bare_number_does_not() {
        let m = Metrics::new()
            .bytes("size", 10usize)
            .count("items", 3.0)
            .ratio("ratio", 2)
            .percent("share", 12.5)
            .add("setup", Duration::from_millis(5))
            .add("plain", 7);
        let got: Vec<_> = m.iter().map(|(n, v)| (n, v.to_string())).collect();
        assert_eq!(
            got,
            [
                ("size", "10B".to_string()),
                ("items", "3".to_string()),
                ("ratio", "2.00".to_string()),
                ("share", "12.5%".to_string()),
                ("setup", "5.00ms".to_string()),
                ("plain", "7".to_string()),
            ]
        );
    }

    #[test]
    fn a_name_given_twice_keeps_the_later_value_in_the_earlier_place() {
        let m = Metrics::new()
            .bytes("a", 1usize)
            .bytes("b", 2usize)
            .bytes("a", 3usize);
        let values: Vec<_> = m.iter().map(|(name, v)| (name, v.as_f64())).collect();
        assert_eq!(values, [("a", 3.0), ("b", 2.0)]);
    }

    #[test]
    fn an_absent_metric_is_simply_absent() {
        let m = Metrics::new().bytes("x", 4u8);
        assert_eq!(m.get("size"), None);
        assert_eq!(m.get("x"), Some(MetricValue::bytes(4)));
        assert!(Metrics::new().is_empty());
        assert!(!m.is_empty());
    }

    #[test]
    fn counted_metrics_wait_for_the_run_and_keep_their_place() {
        let mut m = Metrics::new()
            .bytes("size", 10usize)
            .peak_allocated_bytes()
            .count("items", 3u32)
            .allocation_count()
            .total_allocated_bytes()
            .net_allocated_bytes();
        assert!(m.wants_allocation());
        // Until the run has been counted, they have no value to read.
        assert_eq!(m.get("alloc peak"), None);
        m.resolve_allocation(Some(crate::alloc::Allocations {
            peak_allocated_bytes: 400,
            allocation_count: 7,
            total_allocated_bytes: 900,
            net_allocated_bytes: -120,
        }));
        assert!(!m.wants_allocation());
        let got: Vec<_> = m.iter().collect();
        assert_eq!(
            got,
            [
                ("size", MetricValue::bytes(10)),
                ("alloc peak", MetricValue::bytes(400)),
                ("items", MetricValue::count(3)),
                ("alloc count", MetricValue::count(7)),
                ("alloc total", MetricValue::bytes(900)),
                ("alloc net", MetricValue::bytes(-120)),
            ]
        );
    }

    #[test]
    fn a_value_given_outright_replaces_a_counted_one() {
        let mut m = Metrics::new()
            .peak_allocated_bytes()
            .bytes("alloc peak", 5usize);
        assert!(!m.wants_allocation());
        m.resolve_allocation(None);
        assert_eq!(m.get("alloc peak"), Some(MetricValue::bytes(5)));
    }

    #[test]
    fn nothing_to_resolve_needs_no_counts() {
        let mut m = Metrics::new().bytes("size", 1usize);
        m.resolve_allocation(None);
        assert_eq!(m.get("size"), Some(MetricValue::bytes(1)));
    }

    #[test]
    #[should_panic(expected = "say `allocation`")]
    fn asking_for_counts_from_an_uncounted_run_is_an_error() {
        Metrics::new()
            .peak_allocated_bytes()
            .resolve_allocation(None);
    }

    #[test]
    fn allocations_are_available_to_a_metrics_function_while_it_runs() {
        assert_eq!(Metrics::allocations(), None);
        let stats = crate::alloc::Allocations {
            net_allocated_bytes: 42,
            ..Default::default()
        };
        let provided = crate::alloc::provide(Some(stats));
        assert_eq!(Metrics::allocations(), Some(stats));
        drop(provided);
        assert_eq!(Metrics::allocations(), None);
    }
}
