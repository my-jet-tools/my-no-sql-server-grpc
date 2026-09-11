use std::fmt::Display;

/// The Prometheus text exposition format, written by hand.
///
/// No `prometheus` crate: its value is the registry, and there is nothing to
/// register here - every number is read from live state when the scrape asks for
/// it. What is left is a text format small enough to write, and writing it keeps
/// an encoding fault from being an `unwrap` in the middle of a scrape.
pub struct MetricsWriter {
    out: String,
}

pub enum MetricKind {
    Gauge,
    Counter,
}

impl MetricKind {
    fn as_str(&self) -> &'static str {
        match self {
            MetricKind::Gauge => "gauge",
            MetricKind::Counter => "counter",
        }
    }
}

impl MetricsWriter {
    pub fn new() -> Self {
        Self { out: String::new() }
    }

    /// One metric family: its two header lines, then its samples. Writing them
    /// through here is what keeps a family from being declared twice or a sample
    /// from being emitted without its `# TYPE`.
    pub fn family(
        &mut self,
        name: &str,
        help: &str,
        kind: MetricKind,
        write: impl FnOnce(&mut FamilyWriter),
    ) {
        self.out.push_str("# HELP ");
        self.out.push_str(name);
        self.out.push(' ');
        self.out.push_str(help);
        self.out.push('\n');

        self.out.push_str("# TYPE ");
        self.out.push_str(name);
        self.out.push(' ');
        self.out.push_str(kind.as_str());
        self.out.push('\n');

        let mut family = FamilyWriter {
            out: &mut self.out,
            name,
        };

        write(&mut family);
    }

    pub fn build(self) -> String {
        self.out
    }
}

impl Default for MetricsWriter {
    fn default() -> Self {
        Self::new()
    }
}

pub struct FamilyWriter<'s> {
    out: &'s mut String,
    name: &'s str,
}

impl FamilyWriter<'_> {
    pub fn sample(&mut self, labels: &[(&str, &str)], value: impl Display) {
        self.out.push_str(self.name);

        if !labels.is_empty() {
            self.out.push('{');

            for (at, (label, label_value)) in labels.iter().enumerate() {
                if at > 0 {
                    self.out.push(',');
                }

                self.out.push_str(label);
                self.out.push_str("=\"");
                write_escaped(self.out, label_value);
                self.out.push('"');
            }

            self.out.push('}');
        }

        self.out.push(' ');
        self.out.push_str(&value.to_string());
        self.out.push('\n');
    }
}

/// A label value carries a table name, and table names are not validated
/// anywhere - a client may create one with a quote in it. Unescaped, that single
/// name would make the whole scrape unparseable for every other metric too.
fn write_escaped(dest: &mut String, src: &str) {
    for char in src.chars() {
        match char {
            '\\' => dest.push_str("\\\\"),
            '"' => dest.push_str("\\\""),
            '\n' => dest.push_str("\\n"),
            _ => dest.push(char),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_family_carries_its_header_and_its_samples() {
        let mut writer = MetricsWriter::new();

        writer.family("some_total", "How many", MetricKind::Counter, |family| {
            family.sample(&[("ns", "default")], 42);
            family.sample(&[], 7);
        });

        assert_eq!(
            writer.build(),
            "# HELP some_total How many\n\
             # TYPE some_total counter\n\
             some_total{ns=\"default\"} 42\n\
             some_total 7\n"
        );
    }

    #[test]
    fn a_table_name_can_not_break_the_scrape() {
        let mut writer = MetricsWriter::new();

        writer.family("rows", "Rows", MetricKind::Gauge, |family| {
            family.sample(&[("table", "a\"b\\c\nd")], 1);
        });

        assert_eq!(
            writer.build().lines().last().unwrap(),
            "rows{table=\"a\\\"b\\\\c\\nd\"} 1"
        );
    }
}
