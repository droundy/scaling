use crate::{Group, Measurement, Report, TypedInput};
use std::collections::BTreeSet;

const MAX_TABLE_WIDTH: usize = 100;

pub(crate) fn table(report: &Report) -> String {
    let mut out = String::new();
    for (name, group) in report.groups() {
        let shown = render_group(name, group);
        if !shown.is_empty() {
            out.push_str(&shown);
            out.push('\n');
        }
    }
    out
}

fn render_group(name: &str, group: &Group) -> String {
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

    let title = if types.len() == 1 {
        let type_name = types.first().copied().unwrap_or_default();
        if type_name.is_empty() || type_name == "()" {
            name.to_string()
        } else {
            format!("{name} ({type_name})")
        }
    } else {
        name.to_string()
    };
    let title = with_baseline(title, group, &input_columns);
    if let Some(shown) = render_matrix(&title, "candidate", &group.candidates, &labels, &values) {
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

    render_long(name, group)
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
        "input",
        &transposed_rows,
        &transposed_columns,
        &transposed_values,
    );
    (transpose_width <= MAX_TABLE_WIDTH).then(|| {
        format_matrix(
            title,
            "input",
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

fn render_long(name: &str, group: &Group) -> String {
    let mut rows = Vec::new();
    for (candidate, candidate_name) in group.candidates.iter().enumerate() {
        for (input, details) in group.inputs.iter().enumerate() {
            if let Some(value) = group.measurements[candidate][input] {
                rows.push((
                    candidate_name.as_str(),
                    details.type_name.as_str(),
                    details.name.as_str(),
                    Format(Some(value)).to_string(),
                ));
            }
        }
    }
    let candidate_width = rows.iter().map(|row| row.0.len()).max().unwrap_or(9).max(9);
    let type_width = rows.iter().map(|row| row.1.len()).max().unwrap_or(4).max(4);
    let input_width = rows.iter().map(|row| row.2.len()).max().unwrap_or(5).max(5);
    let all_columns: Vec<usize> = (0..group.inputs.len()).collect();
    let title = with_baseline(name.to_string(), group, &all_columns);
    let mut out = format!(
        "{title}\n{:<candidate_width$}  {:<type_width$}  {:<input_width$}  measurement\n",
        "candidate", "type", "input"
    );
    let mut last_candidate = "";
    let mut last_type_name = "";
    for (mut candidate, mut type_name, input, value) in rows {
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
        out.push_str(&format!("{candidate:<candidate_width$}  {type_name:<type_width$}  {input:<input_width$}  {value}\n"));
    }
    out
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
    use crate::Difference;

    use super::*;
    use expect_test::expect;

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
            candidates: vec!["sort".into()],
            inputs: (0..8)
                .map(|i| TypedInput {
                    name: format!("input_{i}_with_a_long_descriptive_name"),
                    type_name: "Vec<u64>".into(),
                })
                .collect(),
            measurements: vec![vec![Some(timing(22.0)); 8]],
            baselines: vec![None; 8],
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
                candidates,
                inputs,
                measurements,
                baselines: vec![None; 4],
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
            candidates: vec!["stable".into(), "unstable".into()],
            inputs: vec![TypedInput {
                name: String::new(),
                type_name: String::new(),
            }],
            measurements: vec![vec![Some(timing(22.0))], vec![Some(change(30.0, 10.0))]],
            baselines: vec![Some(0)],
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
            candidates: vec!["fast".into(), "small".into()],
            inputs: vec![typed("random", "i32"), typed("random", "u32")],
            measurements: vec![
                vec![None, Some(timing(20.0))],
                vec![Some(timing(5.0)), Some(change(25.0, 5.0))],
            ],
            baselines: vec![None, Some(0)],
        };
        let shown = render_group("codec", &group);
        assert!(shown.starts_with("codec  baseline: fast\n"), "{shown}");
    }

    #[test]
    fn a_lone_benchmark_prints_on_one_line() {
        let group = Group {
            candidates: vec!["lonely".into()],
            inputs: vec![TypedInput::default()],
            measurements: vec![vec![Some(timing(22.0))]],
            baselines: vec![None],
        };
        assert_eq!(render_group("lonely", &group), "lonely  22.0ns ± 0.2ns\n");
    }
}
