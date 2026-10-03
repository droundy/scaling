use crate::{Group, Measurement, TypedInput};
use std::collections::BTreeSet;

const MAX_TABLE_WIDTH: usize = 100;

/// A group as its table. `figures` is how many significant figures to show
/// the metrics to, and the default if there is none; timings always show the
/// digits their error justifies.
pub(crate) fn render_group(name: &str, group: &Group, figures: Option<usize>) -> String {
    if !group.metrics.is_empty() {
        if stacks(group) {
            if let Some(shown) = render_stacked(name, group, figures) {
                return shown;
            }
        }
        return render_with_metrics(name, group, figures);
    }
    if group.candidates.len() == 1 && group.inputs.len() == 1 {
        let input = &group.inputs[0];
        if input.name.is_empty() {
            let value = Format(group.measurements[0][0]);
            return format!("{name}  {value}\n");
        }
    }

    let types: BTreeSet<&str> = group
        .inputs
        .iter()
        .map(|input| input.type_name.as_str())
        .collect();
    let candidate_rows: Vec<usize> = (0..group.candidates.len()).collect();
    let input_columns: Vec<usize> = (0..group.inputs.len()).collect();
    let labels: Vec<String> = group
        .inputs
        .iter()
        .map(|input| input_label(input, types.len() > 1))
        .collect();
    let values = matrix_values(group, &candidate_rows, &input_columns);

    let title = with_baseline(grid_title(name, &types), group, &input_columns);
    if let Some(shown) = render_matrix(
        &title,
        "candidate",
        "input",
        &group.candidates,
        &labels,
        &values,
    ) {
        return shown;
    }

    if types.len() > 1 {
        let mut facets = String::new();
        let mut all_fit = true;
        for type_name in types {
            let columns: Vec<usize> = group
                .inputs
                .iter()
                .enumerate()
                .filter_map(|(i, input)| (input.type_name == type_name).then_some(i))
                .collect();
            let rows: Vec<usize> = (0..group.candidates.len())
                .filter(|&row| {
                    columns
                        .iter()
                        .any(|&column| group.measurements[row][column].is_some())
                })
                .collect();
            let facet_labels: Vec<String> = columns
                .iter()
                .map(|&column| input_label(&group.inputs[column], false))
                .collect();
            let facet_values = matrix_values(group, &rows, &columns);
            let candidates: Vec<String> = rows
                .iter()
                .map(|&row| group.candidates[row].clone())
                .collect();
            let title = with_baseline(format!("{name} ({type_name})"), group, &columns);
            match render_matrix(
                &title,
                "candidate",
                "input",
                &candidates,
                &facet_labels,
                &facet_values,
            ) {
                Some(shown) => {
                    facets.push_str(&shown);
                    facets.push('\n');
                }
                None => {
                    all_fit = false;
                    break;
                }
            }
        }
        if all_fit {
            return facets;
        }
    }

    let all_columns: Vec<usize> = (0..group.inputs.len()).collect();
    render_long(name, group, &all_columns, figures)
}

/// The title of a grid: the group, and the type of its inputs when they all
/// share one.
fn grid_title(name: &str, types: &BTreeSet<&str>) -> String {
    match types.iter().next() {
        Some(type_name) if types.len() == 1 && !type_name.is_empty() && *type_name != "()" => {
            format!("{name} ({type_name})")
        }
        _ => name.to_string(),
    }
}

/// A title that also says what the percentages beneath it are against.
///
/// Without it a cell like `-47.6%` is a number with no referent: the
/// baseline's own cell is an absolute time that looks like any other.
fn with_baseline(title: String, group: &Group, columns: &[usize]) -> String {
    let mut baselines: Vec<(&str, &str)> = Vec::new();
    for &column in columns {
        if let Some(row) = group.baselines[column] {
            let baseline = (
                group.candidates[row].as_str(),
                group.inputs[column].type_name.as_str(),
            );
            if !baselines.contains(&baseline) {
                baselines.push(baseline);
            }
        }
    }
    let Some(&(first, _)) = baselines.first() else {
        return title;
    };
    if baselines.iter().all(|(name, _)| *name == first) {
        return format!("{title}  baseline: {first}");
    }
    // Each type is its own comparison, so say which baseline goes with
    // which.
    let each: Vec<String> = baselines
        .iter()
        .map(|(name, type_name)| format!("{name} ({type_name})"))
        .collect();
    format!("{title}  baselines: {}", each.join(", "))
}

fn input_label(input: &TypedInput, show_type: bool) -> String {
    let name = if input.name.is_empty() {
        "time"
    } else {
        input.name.as_str()
    };
    if show_type && !input.type_name.is_empty() && input.type_name != "()" {
        format!("{name}<{}>", input.type_name)
    } else {
        name.to_string()
    }
}

fn matrix_values(group: &Group, rows: &[usize], columns: &[usize]) -> Vec<Vec<String>> {
    rows.iter()
        .map(|&row| {
            columns
                .iter()
                .map(|&column| Format(group.measurements[row][column]).to_string())
                .collect()
        })
        .collect()
}

fn render_matrix(
    title: &str,
    row_heading: &str,
    transposed_heading: &str,
    rows: &[String],
    columns: &[String],
    values: &[Vec<String>],
) -> Option<String> {
    let direct_width = table_width(row_heading, rows, columns, values);
    if direct_width <= MAX_TABLE_WIDTH {
        return Some(format_matrix(title, row_heading, rows, columns, values));
    }

    let transposed_rows = columns.to_vec();
    let transposed_columns = rows.to_vec();
    let transposed_values: Vec<Vec<String>> = (0..columns.len())
        .map(|column| values.iter().map(|row| row[column].clone()).collect())
        .collect();
    let transpose_width = table_width(
        transposed_heading,
        &transposed_rows,
        &transposed_columns,
        &transposed_values,
    );
    (transpose_width <= MAX_TABLE_WIDTH).then(|| {
        format_matrix(
            title,
            transposed_heading,
            &transposed_rows,
            &transposed_columns,
            &transposed_values,
        )
    })
}

fn table_width(
    row_heading: &str,
    rows: &[String],
    columns: &[String],
    values: &[Vec<String>],
) -> usize {
    let label_width = rows
        .iter()
        .map(String::len)
        .chain(std::iter::once(row_heading.len()))
        .max()
        .unwrap_or(0);
    label_width
        + 2
        + columns
            .iter()
            .enumerate()
            .map(|(column, label)| {
                label.len().max(
                    values
                        .iter()
                        .map(|row| row[column].len())
                        .max()
                        .unwrap_or(0),
                ) + 2
            })
            .sum::<usize>()
}

fn format_matrix(
    title: &str,
    row_heading: &str,
    rows: &[String],
    columns: &[String],
    values: &[Vec<String>],
) -> String {
    let label_width = rows
        .iter()
        .map(String::len)
        .chain(std::iter::once(row_heading.len()))
        .max()
        .unwrap_or(0);
    let widths: Vec<usize> = columns
        .iter()
        .enumerate()
        .map(|(column, label)| {
            label.len().max(
                values
                    .iter()
                    .map(|row| row[column].len())
                    .max()
                    .unwrap_or(0),
            ) + 2
        })
        .collect();
    let mut out = format!("{title}\n{row_heading:<label_width$}");
    for (column, width) in columns.iter().zip(&widths) {
        out.push_str(&format!("{column:>width$}"));
    }
    out.push('\n');
    for (label, row) in rows.iter().zip(values) {
        out.push_str(&format!("{label:<label_width$}"));
        for (value, width) in row.iter().zip(&widths) {
            out.push_str(&format!("{value:>width$}"));
        }
        out.push('\n');
    }
    out
}

/// Every measurement of the given input columns, one to a line.
///
/// The form that always fits, since it grows downwards: used when neither
/// orientation of a table does. Any metrics follow the measurement as
/// further columns.
fn render_long(name: &str, group: &Group, columns: &[usize], figures: Option<usize>) -> String {
    let mut rows = Vec::new();
    for (candidate, candidate_name) in group.candidates.iter().enumerate() {
        for &input in columns {
            let details = &group.inputs[input];
            let shown = group.measurements[candidate][input];
            let metrics: Vec<String> = (0..group.metrics.len())
                .map(|metric| metric_cell(group, metric, candidate, input, figures))
                .collect();
            if shown.is_none() && metrics.iter().all(|m| m == "-") {
                continue;
            }
            rows.push((
                candidate_name.as_str(),
                details.type_name.as_str(),
                details.name.as_str(),
                Format(shown).to_string(),
                metrics,
            ));
        }
    }
    let candidate_width = rows.iter().map(|row| row.0.len()).max().unwrap_or(9).max(9);
    let type_width = rows.iter().map(|row| row.1.len()).max().unwrap_or(4).max(4);
    let input_width = rows.iter().map(|row| row.2.len()).max().unwrap_or(5).max(5);
    let measurement_width = if group.metrics.is_empty() {
        0
    } else {
        rows.iter()
            .map(|row| row.3.len())
            .max()
            .unwrap_or(0)
            .max(11)
    };
    let metric_widths: Vec<usize> = group
        .metrics
        .iter()
        .enumerate()
        .map(|(i, metric)| {
            rows.iter()
                .map(|row| row.4[i].len())
                .max()
                .unwrap_or(0)
                .max(metric.name.len())
        })
        .collect();
    let title = with_baseline(name.to_string(), group, columns);
    let mut out = format!(
        "{title}\n{:<candidate_width$}  {:<type_width$}  {:<input_width$}  {:<measurement_width$}",
        "candidate", "type", "input", "measurement"
    );
    for (metric, width) in group.metrics.iter().zip(&metric_widths) {
        out.push_str(&format!("  {:>width$}", metric.name));
    }
    out.push('\n');
    let mut last_candidate = "";
    let mut last_type_name = "";
    for (mut candidate, mut type_name, input, value, metrics) in rows {
        if candidate == last_candidate {
            candidate = "";
            if last_type_name == type_name {
                type_name = "";
            } else {
                last_type_name = type_name;
            }
        } else {
            last_candidate = candidate;
            last_type_name = type_name;
        }
        out.push_str(&format!(
            "{candidate:<candidate_width$}  {type_name:<type_width$}  {input:<input_width$}  {value:<measurement_width$}"
        ));
        for (metric, width) in metrics.iter().zip(&metric_widths) {
            out.push_str(&format!("  {metric:>width$}"));
        }
        out.push('\n');
    }
    out
}

/// Whether a group's metrics go under its times in one grid.
///
/// That keeps the candidates-down, inputs-across grid a group without
/// metrics has, which reads well while there are few metrics to stack. With
/// one input there is no grid to keep, and with many metrics the stack under
/// each time grows tall, so those take a table to an input instead.
fn stacks(group: &Group) -> bool {
    group.inputs.len() >= 2 && group.metrics.len() <= 2
}

/// The grid of a group with a few metrics: each candidate's time, with its
/// metrics on the lines beneath, named in the margin.
///
/// `None` when it is too wide for the page. Unlike a plain grid it is not
/// turned on its side, because then the metrics would be the columns and the
/// lines under each time would no longer line up.
fn render_stacked(name: &str, group: &Group, figures: Option<usize>) -> Option<String> {
    let types: BTreeSet<&str> = group
        .inputs
        .iter()
        .map(|input| input.type_name.as_str())
        .collect();
    let labels: Vec<String> = group
        .inputs
        .iter()
        .map(|input| input_label(input, types.len() > 1))
        .collect();
    let columns: Vec<usize> = (0..group.inputs.len()).collect();
    let mut lines: Vec<String> = Vec::new();
    let mut values: Vec<Vec<String>> = Vec::new();
    for (row, candidate) in group.candidates.iter().enumerate() {
        lines.push(candidate.clone());
        values.push(
            columns
                .iter()
                .map(|&column| Format(group.measurements[row][column]).to_string())
                .collect(),
        );
        for (metric, column_of_metric) in group.metrics.iter().enumerate() {
            lines.push(format!("  {}", column_of_metric.name));
            values.push(
                columns
                    .iter()
                    .map(|&column| metric_cell(group, metric, row, column, figures))
                    .collect(),
            );
        }
    }
    if table_width("candidate", &lines, &labels, &values) > MAX_TABLE_WIDTH {
        return None;
    }
    let title = with_baseline(grid_title(name, &types), group, &columns);
    Some(format_matrix(&title, "candidate", &lines, &labels, &values))
}

/// A group with metrics, as one table for each input: the candidates down
/// the side, the time and then each metric across.
///
/// A table to an input rather than one grid because the metrics are what
/// there is to read across, and a cell that held a time and several metrics
/// for each input would not be readable.
fn render_with_metrics(name: &str, group: &Group, figures: Option<usize>) -> String {
    let mut out = String::new();
    for column in 0..group.inputs.len() {
        let rows: Vec<usize> = (0..group.candidates.len())
            .filter(|&row| {
                group.measurements[row][column].is_some()
                    || group
                        .metrics
                        .iter()
                        .any(|metric| metric.values[row][column].is_some())
            })
            .collect();
        if rows.is_empty() {
            continue;
        }
        let title = with_baseline(input_title(name, &group.inputs[column]), group, &[column]);
        let headings: Vec<String> = std::iter::once("time".to_string())
            .chain(group.metrics.iter().map(|metric| metric.name.clone()))
            .collect();
        let candidates: Vec<String> = rows
            .iter()
            .map(|&row| group.candidates[row].clone())
            .collect();
        let values: Vec<Vec<String>> = rows
            .iter()
            .map(|&row| {
                std::iter::once(Format(group.measurements[row][column]).to_string())
                    .chain(
                        (0..group.metrics.len())
                            .map(|metric| metric_cell(group, metric, row, column, figures)),
                    )
                    .collect()
            })
            .collect();
        let shown = render_matrix(
            &title,
            "candidate",
            "metric",
            &candidates,
            &headings,
            &values,
        )
        .unwrap_or_else(|| render_long(name, group, &[column], figures));
        if !out.is_empty() {
            out.push('\n');
        }
        out.push_str(&shown);
    }
    out
}

/// `group@input (type)`, naming one input's table.
fn input_title(name: &str, input: &TypedInput) -> String {
    let mut title = name.to_string();
    if !input.name.is_empty() {
        title.push('@');
        title.push_str(&input.name);
    }
    if !input.type_name.is_empty() && input.type_name != "()" {
        title.push_str(&format!(" ({})", input.type_name));
    }
    title
}

/// One metric of one cell as a table shows it: the value, and how it differs
/// from the baseline's when it is not the baseline's.
///
/// The difference needs no significance test, since a metric is computed
/// rather than sampled, so it is always shown.
fn metric_cell(
    group: &Group,
    metric: usize,
    row: usize,
    column: usize,
    figures: Option<usize>,
) -> String {
    let metric = &group.metrics[metric];
    let Some(value) = metric.values[row][column] else {
        return "-".to_string();
    };
    let mut shown = match figures {
        Some(figures) => format!("{value:.figures$}"),
        None => value.to_string(),
    };
    if let Some(baseline) = group.baselines[column].filter(|&baseline| baseline != row) {
        // A difference needs both to be counted in the same unit.
        if let Some(base) = metric.values[baseline][column]
            .filter(|base| value.same_kind(base) && base.as_f64() != 0.0)
        {
            let base = base.as_f64();
            let percent = (value.as_f64() - base) / base.abs() * 100.0;
            if percent.is_finite() {
                let digits = if percent.abs() < 10.0 { 1 } else { 0 };
                shown.push_str(&format!(" ({percent:+.digits$}%)"));
            }
        }
    }
    shown
}

struct Format(Option<Measurement>);

impl std::fmt::Display for Format {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.0 {
            None => f.write_str("-"),
            Some(Measurement::Timing(timing)) => write!(f, "{timing}"),
            Some(Measurement::Scaling(scaling)) => write!(f, "{scaling}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::{Difference, MetricColumn, MetricValue};

    use super::*;
    use expect_test::expect;

    /// A group as its table, with the default figures for its metrics.
    fn render_group(name: &str, group: &Group) -> String {
        super::render_group(name, group, None)
    }

    fn timing(ns_per_iter: f64) -> Measurement {
        Measurement::Timing(crate::Timing {
            ns_per_iter,
            std_error: ns_per_iter * 0.01,
            iterations: 1,
            samples: 2,
            hit_limit: false,
            untrustworthy: false,
            difference: None,
        })
    }

    fn change(ns_per_iter: f64, diff: f64) -> Measurement {
        let Measurement::Timing(baseline) = timing(ns_per_iter - diff) else {
            panic!()
        };
        let Measurement::Timing(mut candidate) = timing(ns_per_iter) else {
            panic!()
        };
        candidate.difference = Some(Difference::from_parts(&baseline, &candidate, 0.5, 0.12));
        Measurement::Timing(candidate)
    }

    #[test]
    fn compact_groups_render_as_grids() {
        let group = Group {
            name: String::new(),
            candidates: vec!["stable".into(), "unstable".into(), "crazy".into()],
            inputs: vec![
                TypedInput {
                    name: "reversed".into(),
                    type_name: "Vec<u64>".into(),
                },
                TypedInput {
                    name: "shuffled".into(),
                    type_name: "Vec<u64>".into(),
                },
            ],
            measurements: vec![
                vec![Some(timing(22.0)), Some(timing(12.0))],
                vec![Some(change(11.0, -10.0)), Some(change(12.01, 0.01))],
                vec![Some(change(22.01, 0.01)), Some(change(10.1, -1.9))],
            ],
            baselines: vec![Some(0), Some(0)],
            metrics: Vec::new(),
        };
        let shown = render_group("sorting", &group);
        expect![[r#"
            sorting (Vec<u64>)  baseline: stable
            candidate         reversed           shuffled
            stable      22.0ns ± 0.2ns   12.00ns ± 0.12ns
            unstable     -47.6% ± 0.6%           (< 0.5%)
            crazy             (< 0.3%)      -15.8% ± 1.0%
        "#]]
        .assert_eq(&shown);
    }

    #[test]
    fn narrow_layout_transposes_when_that_fits() {
        let group = Group {
            name: String::new(),
            candidates: vec!["sort".into()],
            inputs: (0..8)
                .map(|i| TypedInput {
                    name: format!("input_{i}_with_a_long_descriptive_name"),
                    type_name: "Vec<u64>".into(),
                })
                .collect(),
            measurements: vec![vec![Some(timing(22.0)); 8]],
            baselines: vec![None; 8],
            metrics: Vec::new(),
        };
        let shown = render_group("sorting", &group);
        expect![[r#"
            sorting (Vec<u64>)
            input                                            sort
            input_0_with_a_long_descriptive_name   22.0ns ± 0.2ns
            input_1_with_a_long_descriptive_name   22.0ns ± 0.2ns
            input_2_with_a_long_descriptive_name   22.0ns ± 0.2ns
            input_3_with_a_long_descriptive_name   22.0ns ± 0.2ns
            input_4_with_a_long_descriptive_name   22.0ns ± 0.2ns
            input_5_with_a_long_descriptive_name   22.0ns ± 0.2ns
            input_6_with_a_long_descriptive_name   22.0ns ± 0.2ns
            input_7_with_a_long_descriptive_name   22.0ns ± 0.2ns
        "#]]
        .assert_eq(&shown);
    }

    #[test]
    fn long_form_handles_wide_axes() {
        let candidates: Vec<String> = (0..4)
            .map(|i| format!("candidate_{i}_with_a_long_name"))
            .collect();
        let inputs: Vec<TypedInput> = (0..4)
            .map(|i| TypedInput {
                name: format!("input_{i}_with_a_long_descriptive_name"),
                type_name: "Vec<u64>".into(),
            })
            .collect();
        let measurements = candidates
            .iter()
            .map(|_| {
                (0..inputs.len())
                    .map(|i| Some(timing(10.0 + i as f64)))
                    .collect()
            })
            .collect();
        let shown = render_group(
            "wide",
            &Group {
                name: String::new(),
                candidates,
                inputs,
                measurements,
                baselines: vec![None; 4],
                metrics: Vec::new(),
            },
        );
        expect![[r#"
            wide
            candidate                     type      input                                 measurement
            candidate_0_with_a_long_name  Vec<u64>  input_0_with_a_long_descriptive_name  10.00ns ± 0.10ns
                                                    input_1_with_a_long_descriptive_name  11.00ns ± 0.11ns
                                                    input_2_with_a_long_descriptive_name  12.00ns ± 0.12ns
                                                    input_3_with_a_long_descriptive_name  13.00ns ± 0.13ns
            candidate_1_with_a_long_name  Vec<u64>  input_0_with_a_long_descriptive_name  10.00ns ± 0.10ns
                                                    input_1_with_a_long_descriptive_name  11.00ns ± 0.11ns
                                                    input_2_with_a_long_descriptive_name  12.00ns ± 0.12ns
                                                    input_3_with_a_long_descriptive_name  13.00ns ± 0.13ns
            candidate_2_with_a_long_name  Vec<u64>  input_0_with_a_long_descriptive_name  10.00ns ± 0.10ns
                                                    input_1_with_a_long_descriptive_name  11.00ns ± 0.11ns
                                                    input_2_with_a_long_descriptive_name  12.00ns ± 0.12ns
                                                    input_3_with_a_long_descriptive_name  13.00ns ± 0.13ns
            candidate_3_with_a_long_name  Vec<u64>  input_0_with_a_long_descriptive_name  10.00ns ± 0.10ns
                                                    input_1_with_a_long_descriptive_name  11.00ns ± 0.11ns
                                                    input_2_with_a_long_descriptive_name  12.00ns ± 0.12ns
                                                    input_3_with_a_long_descriptive_name  13.00ns ± 0.13ns
        "#]].assert_eq(&shown);
    }

    #[test]
    fn a_comparison_names_its_baseline() {
        let group = Group {
            name: String::new(),
            candidates: vec!["stable".into(), "unstable".into()],
            inputs: vec![TypedInput {
                name: String::new(),
                type_name: String::new(),
            }],
            measurements: vec![vec![Some(timing(22.0))], vec![Some(change(30.0, 10.0))]],
            baselines: vec![Some(0)],
            metrics: Vec::new(),
        };
        let shown = render_group("sorting", &group);
        assert!(shown.starts_with("sorting  baseline: stable\n"), "{shown}");
        // A slowdown is marked as plainly as a speedup.
        assert!(shown.contains("+"), "{shown}");
    }

    #[test]
    fn each_type_names_its_own_baseline() {
        let typed = |name: &str, type_name: &str| TypedInput {
            name: name.into(),
            type_name: type_name.into(),
        };
        let group = Group {
            name: String::new(),
            candidates: vec!["fast".into(), "small".into()],
            inputs: vec![typed("random", "i32"), typed("random", "u32")],
            measurements: vec![
                vec![None, Some(timing(20.0))],
                vec![Some(timing(5.0)), Some(change(25.0, 5.0))],
            ],
            baselines: vec![None, Some(0)],
            metrics: Vec::new(),
        };
        let shown = render_group("codec", &group);
        assert!(shown.starts_with("codec  baseline: fast\n"), "{shown}");
    }

    #[test]
    fn a_lone_benchmark_prints_on_one_line() {
        let group = Group {
            name: String::new(),
            candidates: vec!["lonely".into()],
            inputs: vec![TypedInput::default()],
            measurements: vec![vec![Some(timing(22.0))]],
            baselines: vec![None],
            metrics: Vec::new(),
        };
        assert_eq!(render_group("lonely", &group), "lonely  22.0ns ± 0.2ns\n");
    }

    fn column(name: &str, values: Vec<Vec<Option<MetricValue>>>) -> MetricColumn {
        MetricColumn {
            name: name.into(),
            values,
        }
    }

    fn bytes(size: f64) -> Option<MetricValue> {
        Some(MetricValue::bytes(size))
    }

    fn serializers() -> Group {
        Group {
            name: String::new(),
            candidates: vec!["json".into(), "postcard".into(), "bincode".into()],
            inputs: vec![TypedInput {
                name: "mesh".into(),
                type_name: "Mesh".into(),
            }],
            measurements: vec![
                vec![Some(timing(81.2))],
                vec![Some(change(31.6, -49.6))],
                vec![Some(change(32.5, -48.7))],
            ],
            baselines: vec![Some(0)],
            metrics: vec![
                column(
                    "size",
                    vec![
                        vec![bytes(1992294.0)],
                        vec![bytes(755_000.0)],
                        vec![bytes(802_000.0)],
                    ],
                ),
                column(
                    "ratio",
                    vec![
                        vec![Some(MetricValue::ratio(0.31))],
                        vec![Some(MetricValue::ratio(0.52))],
                        vec![None],
                    ],
                ),
            ],
        }
    }

    #[test]
    fn metrics_become_columns_beside_the_time() {
        let shown = render_group("serialize", &serializers());
        expect![[r#"
            serialize@mesh (Mesh)  baseline: json
            candidate              time           size         ratio
            json         81.2ns ± 0.8ns        1.90MiB         0.310
            postcard    -61.08% ± 0.15%  737KiB (-62%)  0.520 (+68%)
            bincode     -59.98% ± 0.15%  783KiB (-60%)             -
        "#]]
        .assert_eq(&shown);
    }

    #[test]
    fn each_input_gets_its_own_table_when_there_are_many_metrics() {
        let mut group = serializers();
        add_metrics(&mut group, 1);
        group.inputs.push(TypedInput {
            name: "log".into(),
            type_name: "Log".into(),
        });
        group.baselines.push(Some(0));
        group.measurements[0].push(Some(timing(40.0)));
        group.measurements[1].push(Some(change(30.0, -10.0)));
        group.measurements[2].push(None);
        group.metrics[0].values[0].push(bytes(2048.0));
        group.metrics[0].values[1].push(bytes(512.0));
        group.metrics[0].values[2].push(None);
        group.metrics[1].values[0].push(None);
        group.metrics[1].values[1].push(None);
        group.metrics[1].values[2].push(None);
        for extra in group.metrics.iter_mut().skip(2) {
            for row in &mut extra.values {
                row.push(None);
            }
        }
        let shown = render_group("serialize", &group);
        expect![[r#"
            serialize@mesh (Mesh)  baseline: json
            candidate              time           size         ratio  another_metric_0
            json         81.2ns ± 0.8ns        1.90MiB         0.310                 0
            postcard    -61.08% ± 0.15%  737KiB (-62%)  0.520 (+68%)                 1
            bincode     -59.98% ± 0.15%  783KiB (-60%)             -                 2

            serialize@log (Log)  baseline: json
            candidate             time         size  ratio  another_metric_0
            json        40.0ns ± 0.4ns      2.00KiB      -                 -
            postcard     -25.0% ± 0.3%  512B (-75%)      -                 -
        "#]]
        .assert_eq(&shown);
    }

    #[test]
    fn a_metric_table_too_wide_for_the_page_is_turned_on_its_side() {
        let mut group = serializers();
        add_metrics(&mut group, 5);
        let shown = render_group("serialize", &group);
        expect![[r#"
            serialize@mesh (Mesh)  baseline: json
            metric                       json          postcard           bincode
            time               81.2ns ± 0.8ns   -61.08% ± 0.15%   -59.98% ± 0.15%
            size                      1.90MiB     737KiB (-62%)     783KiB (-60%)
            ratio                       0.310      0.520 (+68%)                 -
            another_metric_0                0                 1                 2
            another_metric_1               10         11 (+10%)         12 (+20%)
            another_metric_2               20        21 (+5.0%)         22 (+10%)
            another_metric_3               30        31 (+3.3%)        32 (+6.7%)
            another_metric_4               40        41 (+2.5%)        42 (+5.0%)
        "#]]
        .assert_eq(&shown);
    }

    #[test]
    fn a_metric_table_too_wide_either_way_falls_back_to_one_line_each() {
        let mut group = serializers();
        add_metrics(&mut group, 5);
        for candidate in &mut group.candidates {
            candidate.push_str("_with_a_name_that_is_far_too_long_for_a_column");
        }
        let shown = render_group("serialize", &group);
        expect![[r#"
            serialize  baseline: json_with_a_name_that_is_far_too_long_for_a_column
            candidate                                               type  input  measurement                size         ratio  another_metric_0  another_metric_1  another_metric_2  another_metric_3  another_metric_4
            json_with_a_name_that_is_far_too_long_for_a_column      Mesh  mesh   81.2ns ± 0.8ns          1.90MiB         0.310                 0                10                20                30                40
            postcard_with_a_name_that_is_far_too_long_for_a_column  Mesh  mesh   -61.08% ± 0.15%   737KiB (-62%)  0.520 (+68%)                 1         11 (+10%)        21 (+5.0%)        31 (+3.3%)        41 (+2.5%)
            bincode_with_a_name_that_is_far_too_long_for_a_column   Mesh  mesh   -59.98% ± 0.15%   783KiB (-60%)             -                 2         12 (+20%)         22 (+10%)        32 (+6.7%)        42 (+5.0%)
        "#]].assert_eq(&shown);
    }

    /// The serializers on a second input, with only the first two metrics.
    fn serializers_on_two_inputs() -> Group {
        let mut group = serializers();
        group.inputs.push(TypedInput {
            name: "log".into(),
            type_name: "Mesh".into(),
        });
        group.baselines.push(Some(0));
        group.measurements[0].push(Some(timing(40.0)));
        group.measurements[1].push(Some(change(30.0, -10.0)));
        group.measurements[2].push(None);
        group.metrics[0].values[0].push(bytes(2048.0));
        group.metrics[0].values[1].push(bytes(512.0));
        group.metrics[0].values[2].push(None);
        group.metrics[1].values[0].push(None);
        group.metrics[1].values[1].push(None);
        group.metrics[1].values[2].push(None);
        group
    }

    #[test]
    fn a_few_metrics_stack_under_each_time() {
        let shown = render_group("serialize", &serializers_on_two_inputs());
        expect![[r#"
            serialize (Mesh)  baseline: json
            candidate              mesh              log
            json         81.2ns ± 0.8ns   40.0ns ± 0.4ns
              size              1.90MiB          2.00KiB
              ratio               0.310                -
            postcard    -61.08% ± 0.15%    -25.0% ± 0.3%
              size        737KiB (-62%)      512B (-75%)
              ratio        0.520 (+68%)                -
            bincode     -59.98% ± 0.15%                -
              size        783KiB (-60%)                -
              ratio                   -                -
        "#]]
        .assert_eq(&shown);
    }

    #[test]
    fn one_metric_stacks_too() {
        let mut group = serializers_on_two_inputs();
        group.metrics.truncate(1);
        let shown = render_group("serialize", &group);
        expect![[r#"
            serialize (Mesh)  baseline: json
            candidate              mesh              log
            json         81.2ns ± 0.8ns   40.0ns ± 0.4ns
              size              1.90MiB          2.00KiB
            postcard    -61.08% ± 0.15%    -25.0% ± 0.3%
              size        737KiB (-62%)      512B (-75%)
            bincode     -59.98% ± 0.15%                -
              size        783KiB (-60%)                -
        "#]]
        .assert_eq(&shown);
    }

    #[test]
    fn one_input_is_a_table_not_a_stack() {
        let shown = render_group("serialize", &serializers());
        assert!(shown.contains("candidate"), "{shown}");
        assert!(!shown.contains("\n  size"), "{shown}");
    }

    #[test]
    fn a_stack_too_wide_for_the_page_becomes_a_table_to_an_input() {
        let mut group = serializers_on_two_inputs();
        group.inputs[0].name = "an_input_with_a_name_much_too_long_for_any_column_to_have".into();
        group.inputs[1].name = "another_input_with_a_name_much_too_long_for_a_column_too".into();
        let shown = render_group("serialize", &group);
        assert!(!shown.contains("\n  size"), "{shown}");
        assert!(shown.contains("serialize@another_input"), "{shown}");
    }

    /// `count` more metrics, each with a name long enough to take room.
    fn add_metrics(group: &mut Group, count: usize) {
        for i in 0..count {
            let values = (0..group.candidates.len())
                .map(|row| vec![Some(MetricValue::count(i * 10 + row))])
                .collect();
            group
                .metrics
                .push(column(&format!("another_metric_{i}"), values));
        }
    }

    /// A difference needs both values counted in the same unit; a cell that
    /// disagrees with its baseline on that is shown as it is, with none.
    #[test]
    fn a_difference_is_shown_only_between_values_of_one_kind() {
        let mut group = serializers();
        group.metrics = vec![column(
            "size",
            vec![
                vec![bytes(2048.0)],
                vec![Some(MetricValue::from(1024))],
                vec![bytes(1024.0)],
            ],
        )];
        let shown = render_group("serialize", &group);
        assert!(shown.contains("2.00KiB"), "{shown}");
        // The count of 1024 is not a size, so it has no percentage against 2KiB.
        let postcard = shown.lines().find(|l| l.starts_with("postcard")).unwrap();
        assert!(postcard.trim_end().ends_with("1024"), "{postcard}");
        // The size is, and halves it.
        let bincode = shown.lines().find(|l| l.starts_with("bincode")).unwrap();
        assert!(bincode.contains("1.00KiB (-50%)"), "{bincode}");
    }

    #[test]
    fn a_lone_benchmark_can_have_metrics() {
        let group = Group {
            name: String::new(),
            candidates: vec!["lonely".into()],
            inputs: vec![TypedInput::default()],
            measurements: vec![vec![Some(timing(22.0))]],
            baselines: vec![None],
            metrics: vec![column(
                "allocations",
                vec![vec![Some(MetricValue::count(3))]],
            )],
        };
        let shown = render_group("lonely", &group);
        expect![[r#"
            lonely
            candidate             time  allocations
            lonely      22.0ns ± 0.2ns            3
        "#]]
        .assert_eq(&shown);
    }
}
