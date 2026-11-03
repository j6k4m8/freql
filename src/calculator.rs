//! Stateful dimensional calculator backed by the freql analyzer.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use num_traits::{One, ToPrimitive};
use thiserror::Error;

use crate::analyzer::{Compilation, Compiler, CompilerOptions};
use crate::diagnostic::Severity;
use crate::parser::parse_display_expression;
use crate::source::load_virtual;
use crate::types::Type;

const SESSION_PROCESS: &str = "CalculatorSession";
const RESULT_BINDING: &str = "__calculator_result";

#[derive(Debug, Error)]
pub enum CalculatorError {
    #[error("{0}")]
    Invalid(String),
    #[error(transparent)]
    Source(#[from] crate::source::SourceError),
    #[error(transparent)]
    Compile(#[from] crate::analyzer::CompileError),
}

/// A persistent calculator scope containing imports, mutable `let`, and `const`
/// bindings. Every input is checked with the ordinary compiler before it is
/// committed, so calculator and `.req` semantics cannot drift apart.
#[derive(Clone, Debug)]
pub struct CalculatorSession {
    base: PathBuf,
    imports: Vec<String>,
    units: Vec<String>,
    bindings: Vec<String>,
    warnings: Vec<String>,
}

impl CalculatorSession {
    /// Create a calculator whose quoted imports resolve relative to `base`.
    pub fn new(base: impl Into<PathBuf>) -> Self {
        Self {
            base: base.into(),
            imports: Vec::new(),
            units: Vec::new(),
            bindings: Vec::new(),
            warnings: Vec::new(),
        }
    }

    pub fn base(&self) -> &Path {
        &self.base
    }

    pub fn imports(&self) -> &[String] {
        &self.imports
    }

    pub fn units(&self) -> &[String] {
        &self.units
    }

    pub fn bindings(&self) -> &[String] {
        &self.bindings
    }

    /// Warnings from the most recent input, cleared before the next input.
    pub fn warnings(&self) -> &[String] {
        &self.warnings
    }

    pub fn clear(&mut self) {
        self.imports.clear();
        self.units.clear();
        self.bindings.clear();
        self.warnings.clear();
    }

    /// Names suitable for interactive completion in the current scope.
    pub fn completion_names(&self) -> Vec<String> {
        let mut names = [
            "Bool", "Integer", "ceil", "floor", "min", "max", "sum", "math.pi",
        ]
        .into_iter()
        .map(str::to_owned)
        .collect::<BTreeSet<_>>();
        let source = build_source(&self.imports, &self.units, &self.bindings, None, None);
        if let Ok(compilation) = self.compile(&source) {
            // Every built-in unit plus whatever the session's imports declared.
            names.extend(compilation.registry.units.keys().cloned());
            names.extend(compilation.model.units.keys().cloned());
            names.extend(compilation.model.functions.keys().cloned());
            names.extend(compilation.model.artifacts.keys().cloned());
            names.extend(compilation.model.resources.keys().cloned());
            names.extend(compilation.model.processes.keys().cloned());
            if let Some(process) = compilation.model.processes.get(SESSION_PROCESS) {
                names.extend(process.bindings.keys().cloned());
            }
        }
        names.remove(SESSION_PROCESS);
        names.into_iter().collect()
    }

    /// Accept an ordinary freql `import`, `unit`, or `let`, or evaluate a numeric
    /// expression. A trailing `as Unit` selects presentation without changing
    /// the expression's structural type or exact value.
    ///
    /// Successful declarations return `None`; successful expressions return
    /// their exact, unit-normalized display value.
    pub fn execute(&mut self, input: &str) -> Result<Option<String>, CalculatorError> {
        self.warnings.clear();
        let input = input.trim().trim_end_matches(';').trim();
        if input.is_empty() || input.starts_with('#') {
            return Ok(None);
        }
        if starts_with_keyword(input, "import") {
            let mut imports = self.imports.clone();
            imports.push(input.to_owned());
            self.validate_scope(&imports, &self.units, &self.bindings)?;
            self.imports = imports;
            return Ok(None);
        }
        if is_unit_declaration(input) {
            let mut units = self.units.clone();
            units.push(input.to_owned());
            self.validate_scope(&self.imports, &units, &self.bindings)?;
            self.units = units;
            return Ok(None);
        }
        if starts_with_keyword(input, "let")
            || starts_with_keyword(input, "const")
            || is_assignment(input)
        {
            let mut bindings = self.bindings.clone();
            bindings.push(input.to_owned());
            let prefix = build_source(&self.imports, &self.units, &self.bindings, None, None);
            let input_start = prefix.len() - "    }\n}\n".len();
            let source = build_source(&self.imports, &self.units, &bindings, None, None);
            let compilation = self.compile(&source)?;
            ensure_valid(&compilation)?;
            self.warnings = compilation
                .report
                .diagnostics
                .iter()
                .filter(|diagnostic| {
                    diagnostic.severity == Severity::Warning
                        && diagnostic.span.path == "<calculator>"
                        && diagnostic.span.start >= input_start
                })
                .map(|diagnostic| format!("warning[{}]: {}", diagnostic.code, diagnostic.message))
                .collect();
            self.bindings = bindings;
            return Ok(None);
        }

        let parsed = parse_display_expression(input).map_err(|errors| {
            // freql keywords are contextual, so an unsupported declaration
            // parses as a bare identifier and reports against its *second*
            // token. Recognising the leading keyword on the failure path costs
            // nothing and never misroutes a valid expression.
            if let Some(keyword) = unsupported_declaration_keyword(input) {
                return CalculatorError::Invalid(format!(
                    "{keyword} declarations are not supported in the calculator; \
                     define it in a .req file and use `import \"path.req\" (Name)`"
                ));
            }
            CalculatorError::Invalid(
                errors
                    .into_iter()
                    .map(|error| error.to_string())
                    .collect::<Vec<_>>()
                    .join("\n"),
            )
        })?;
        let expression = parsed.expression.display();
        let display_unit = parsed.display_unit;
        let approximate = parsed.approximate;
        let probe = format!("let {RESULT_BINDING} = {expression}");
        let probe_source = build_source(
            &self.imports,
            &self.units,
            &self.bindings,
            None,
            Some(&probe),
        );
        let probe_compilation = self.compile(&probe_source)?;
        ensure_valid(&probe_compilation)?;
        let inferred = probe_compilation
            .model
            .processes
            .get(SESSION_PROCESS)
            .and_then(|process| process.bindings.get(RESULT_BINDING))
            .cloned()
            .ok_or_else(|| CalculatorError::Invalid("could not infer expression type".into()))?;
        if matches!(inferred, Type::Unknown | Type::Error) {
            return Err(CalculatorError::Invalid(
                "expression has an unresolved type".into(),
            ));
        }

        // The probe's inferred display can contain concrete composite units
        // such as `(Gigabyte / Second) * Hour`. Re-declaring that text would
        // accidentally turn an inferred spelling into an explicit formatting
        // request and prevent the shared quantity formatter from cancelling
        // dimensions. Use the neutral structural dimension here; a user-given
        // trailing `as Unit` remains authoritative below.
        let result_type = neutral_type_display(&inferred, &probe_compilation)
            .unwrap_or_else(|| inferred.to_string());
        let presentation = match (approximate, display_unit.as_deref()) {
            (true, Some(unit)) => format!(" as ~{unit}"),
            (true, None) => " as ~".to_owned(),
            (false, Some(unit)) => format!(" as {unit}"),
            (false, None) => String::new(),
        };
        let result_declaration = format!("monitor {RESULT_BINDING}: {result_type}{presentation}");
        let assignment = format!("{RESULT_BINDING} = {expression}");
        let result_source = build_source(
            &self.imports,
            &self.units,
            &self.bindings,
            Some(&result_declaration),
            Some(&assignment),
        );
        let result_compilation = self.compile(&result_source)?;
        ensure_valid(&result_compilation)?;
        result_compilation
            .report
            .monitored_metrics
            .iter()
            .find(|metric| metric.path == format!("{SESSION_PROCESS}.{RESULT_BINDING}"))
            .map(|metric| Some(metric.display_value().to_owned()))
            .ok_or_else(|| CalculatorError::Invalid("expression did not produce a value".into()))
    }

    fn validate_scope(
        &self,
        imports: &[String],
        units: &[String],
        bindings: &[String],
    ) -> Result<(), CalculatorError> {
        let source = build_source(imports, units, bindings, None, None);
        ensure_valid(&self.compile(&source)?)
    }

    fn compile(&self, source: &str) -> Result<Compilation, CalculatorError> {
        let sources = load_virtual(&self.base, source, false)?;
        Ok(Compiler::new(CompilerOptions::default()).check_sources(sources)?)
    }
}

fn neutral_type_display(ty: &Type, compilation: &Compilation) -> Option<String> {
    let Type::Quantity { dimension, .. } = ty else {
        return Some(ty.to_string());
    };
    if dimension.is_scalar() {
        return Some("Scalar".into());
    }

    // Registry-derived rather than a hardcoded symbol table, so a model's own
    // base dimensions and every built-in one are spelled the same way.
    let names = compilation.registry.base_dimension_names();
    let dimension_name = |base: &str| names.get(base).cloned();
    let mut numerator = Vec::new();
    let mut denominator = Vec::new();
    for (base, exponent) in &dimension.0 {
        if !exponent.denom().is_one() {
            return None;
        }
        let power = exponent.numer().to_i32()?;
        if power == 0 {
            continue;
        }
        let name = dimension_name(base)?;
        let magnitude = power.unsigned_abs();
        let factor = if magnitude == 1 {
            name
        } else {
            format!("{name}^{magnitude}")
        };
        if power > 0 {
            numerator.push(factor);
        } else {
            denominator.push(factor);
        }
    }
    let numerator = if numerator.is_empty() {
        // A pure inverse dimension still needs a parsable numerator, and `1`
        // is not one: a type expression cannot begin with a numeric literal.
        // `Scalar` is the dimensionless unit and re-parses correctly.
        "Scalar".to_owned()
    } else {
        numerator.join(" * ")
    };
    Some(if denominator.is_empty() {
        numerator
    } else {
        // The denominator must be parenthesized: `Mass / Length * Time^2`
        // re-parses left to right as `(M / L) * T^2`, inverting the exponent of
        // every factor after the first. Pressure and voltage both have
        // two-factor denominators, so this is not a corner case.
        format!("{numerator} / ({})", denominator.join(" * "))
    })
}

/// Lexing distinguishes assignments from equality and from '=' inside strings
/// or comments. The full compiler still validates the statement and target.
fn is_assignment(input: &str) -> bool {
    use crate::syntax::SyntaxKind as K;
    crate::lexer::lex(input).tokens.iter().any(|token| {
        matches!(
            token.kind,
            K::Eq | K::PlusEq | K::MinusEq | K::StarEq | K::SlashEq | K::CaretEq
        )
    })
}

/// Whether `input` is a `unit <Name> = ...` declaration.
///
/// Checked structurally rather than by leading keyword alone: `unit` is a
/// contextual keyword, so `unit * 2` is a legitimate expression referring to a
/// binding named `unit` and must not be routed here.
fn is_unit_declaration(input: &str) -> bool {
    let Some(tail) = input
        .strip_prefix("unit")
        .filter(|tail| tail.starts_with(char::is_whitespace))
    else {
        return false;
    };
    let tail = tail.trim_start();
    let name_len = tail
        .find(|character: char| !character.is_alphanumeric() && character != '_')
        .unwrap_or(tail.len());
    if name_len == 0 || tail.starts_with(|character: char| character.is_ascii_digit()) {
        return false;
    }
    tail[name_len..].trim_start().starts_with('=')
}

/// The leading item keyword of a declaration the calculator cannot accept.
fn unsupported_declaration_keyword(input: &str) -> Option<&'static str> {
    ["process", "artifact", "resource", "fn", "trait", "impl"]
        .into_iter()
        .find(|keyword| starts_with_keyword(input, keyword))
}

fn starts_with_keyword(input: &str, keyword: &str) -> bool {
    input
        .strip_prefix(keyword)
        .is_some_and(|tail| tail.is_empty() || tail.starts_with(char::is_whitespace))
}

fn build_source(
    imports: &[String],
    units: &[String],
    bindings: &[String],
    member: Option<&str>,
    final_statement: Option<&str>,
) -> String {
    let mut source = String::new();
    for import in imports {
        source.push_str(import);
        source.push('\n');
    }
    // Unit declarations are items, not statements, so they belong at module
    // scope -- and after the imports, since a derived unit may be defined in
    // terms of an imported one.
    for unit in units {
        source.push_str(unit);
        source.push('\n');
    }
    source.push_str(&format!("process {SESSION_PROCESS} {{\n"));
    if let Some(member) = member {
        source.push_str("    ");
        source.push_str(member);
        source.push('\n');
    }
    source.push_str("\n    () => {\n");
    for binding in bindings {
        source.push_str("        ");
        source.push_str(binding);
        source.push('\n');
    }
    if let Some(statement) = final_statement {
        source.push_str("        ");
        source.push_str(statement);
        source.push('\n');
    }
    source.push_str("    }\n}\n");
    source
}

fn ensure_valid(compilation: &Compilation) -> Result<(), CalculatorError> {
    let errors = compilation
        .report
        .diagnostics
        .iter()
        .filter(|diagnostic| diagnostic.severity == Severity::Error)
        .map(|diagnostic| {
            format!(
                "error[{}]: {}\n  --> {}:{}:{}",
                diagnostic.code,
                diagnostic.message,
                diagnostic.span.path,
                diagnostic.span.line,
                diagnostic.span.column
            )
        })
        .collect::<Vec<_>>();
    if errors.is_empty() {
        Ok(())
    } else {
        Err(CalculatorError::Invalid(errors.join("\n")))
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use tempfile::tempdir;

    use super::*;

    #[test]
    fn signed_powers_are_exact_and_dimension_checked() {
        let directory = tempdir().unwrap();
        let mut calculator = CalculatorSession::new(directory.path());
        for (source, expected) in [
            ("1.25^-1", "0.8"),
            ("2^+3", "8"),
            ("(2 Meter)^-2 as Meter^-2", "0.25 Meter^-2"),
            ("3 Meter^-1 as Meter^-1", "3 Meter^-1"),
            ("-2^2", "4"),
            (
                "(2 Meter * 3 Meter * 4 Meter) * (5 Kilogram / Meter^3) * (1.25^-1) as Gram",
                "96000 Gram",
            ),
        ] {
            let result = calculator
                .execute(source)
                .unwrap_or_else(|e| panic!("{source}: {e}"));
            assert_eq!(result, Some(expected.into()), "{source}");
        }
        let invalid = calculator
            .execute("(2 Meter * 3 Meter * 4 Meter) * (5 Kilogram / Meter^3) * (1.25^-1 Gram)")
            .unwrap_err()
            .to_string();
        assert!(
            invalid.contains("exponent must be dimensionless"),
            "{invalid}"
        );
        assert!(invalid.contains("Gram"), "{invalid}");
        assert!(!invalid.contains("E0815"), "must not cascade: {invalid}");
        assert!(
            calculator
                .execute("0^-1")
                .unwrap_err()
                .to_string()
                .contains("zero cannot be raised")
        );
        // A failed input must not damage the calculator session.
        assert_eq!(calculator.execute("2^-1").unwrap(), Some("0.5".into()));
        calculator.execute("unit PerMeter = Meter^-1").unwrap();
        calculator
            .execute("let density: Meter^-1 = 3 PerMeter")
            .unwrap();
        assert_eq!(
            calculator.execute("density as Millimeter^-1").unwrap(),
            Some("0.003 Millimeter^-1".into())
        );
    }

    #[test]
    fn parenthesized_coefficients_accept_unit_suffixes() {
        let directory = tempdir().unwrap();
        let mut calculator = CalculatorSession::new(directory.path());
        for (source, expected) in [
            ("(350 / 24) Day", "175/12 Day"),
            ("(350 / 24) Day as Hour", "350 Hour"),
            ("(2 + 1) Meter^2", "3 Meter^2"),
            ("((2 + 1) Meter)^2", "9 Meter^2"),
            ("(2 + 1) Meter^-1 as Meter^-1", "3 Meter^-1"),
            ("(2 + 1) Meter / Second", "3 Meter / Second"),
        ] {
            assert_eq!(
                calculator.execute(source).unwrap(),
                Some(expected.into()),
                "{source}"
            );
        }
        fs::write(directory.path().join("units.req"), "unit Tick = Second * 2").unwrap();
        calculator.execute("import \"units.req\" as units").unwrap();
        assert_eq!(
            calculator.execute("(2 + 1) units.Tick as Second").unwrap(),
            Some("6 Second".into())
        );
        calculator.execute("let hours = 350 / 24").unwrap();
        assert_eq!(
            calculator.execute("(hours) Day as Hour").unwrap(),
            Some("350 Hour".into())
        );
        assert!(calculator.execute("(1 + 1) MissingUnit").is_err());
        assert!(calculator.execute("(true) Day").is_err());
    }

    #[test]
    fn presentation_inside_result_parentheses() {
        let directory = tempdir().unwrap();
        let mut calculator = CalculatorSession::new(directory.path());
        calculator.execute("let scale = 2").unwrap();
        calculator.execute("unit Item = Count").unwrap();
        for (source, expected) in [
            ("(2 Hour / 3 as Minute)", "40 Minute"),
            ("((2 Hour / 3 as ~Hour))", "≈0.666667 Hour"),
            ("(2 Hour / 3 as ~)", "≈0.666667 Hour"),
            (
                "(12 Meter^3 / ((1*scale) Meter * (1*scale) Meter * (1*scale) Meter) as Item)",
                "1.5 Item",
            ),
        ] {
            assert_eq!(
                calculator.execute(source).unwrap().as_deref(),
                Some(expected),
                "{source}"
            );
        }
        assert!(calculator.execute("(2 Meter as Hour)").is_err());
        assert_eq!(
            calculator.execute("(3 + 1) Meter").unwrap().as_deref(),
            Some("4 Meter")
        );
    }

    #[test]
    fn approximate_display_converts_units_without_changing_exact_values() {
        let directory = tempdir().unwrap();
        let mut calculator = CalculatorSession::new(directory.path());
        for (source, expected) in [
            ("(2 Hour / 4) as Minute", "30 Minute"),
            ("(2 Hour / 4) as ~Minute", "30 Minute"),
            ("(2 Hour / 3) as ~Hour", "≈0.666667 Hour"),
            ("(2 Hour / 3) as ~", "≈0.666667 Hour"),
            ("(2 / 3) as ~", "≈0.666667"),
            ("(-2 Hour / 3) as ~Hour", "≈-0.666667 Hour"),
            (
                "(1 Meter / 3 Second) as ~(Meter / Second)",
                "≈0.333333 Meter / Second",
            ),
        ] {
            assert_eq!(
                calculator.execute(source).unwrap().as_deref(),
                Some(expected),
                "{source}"
            );
        }
        calculator.execute("let duration = 2 Hour / 3").unwrap();
        assert_eq!(
            calculator.execute("duration as ~Hour").unwrap().as_deref(),
            Some("≈0.666667 Hour")
        );
        assert_eq!(
            calculator.execute("duration as Hour").unwrap().as_deref(),
            Some("2/3 Hour")
        );
        assert_eq!(
            calculator.execute("duration as Minute").unwrap().as_deref(),
            Some("40 Minute")
        );
        assert!(calculator.execute("(2 / 3) as ~Hour").is_err());
        assert!(calculator.execute("1 Meter as ~Hour").is_err());
        assert!(calculator.execute("true as ~").is_err());
        calculator.execute("let rounded = 2 Hour / 3 as ~").unwrap();
        assert_eq!(
            calculator.execute("rounded * 3").unwrap().as_deref(),
            Some("2 Hour")
        );
    }

    #[test]
    fn imports_binds_and_converts_exact_quantities() {
        let directory = tempdir().unwrap();
        fs::write(
            directory.path().join("units.req"),
            r#"
                unit Information = base
                unit Byte = Information
                unit Gigabyte = Byte * 1000000000
                unit Petabyte = Byte * 1000000000000000
                unit Hour = Second * 3600
            "#,
        )
        .unwrap();
        let mut calculator = CalculatorSession::new(directory.path());
        calculator
            .execute(r#"import "units.req" (Information, Byte, Gigabyte, Petabyte, Hour)"#)
            .unwrap();
        calculator
            .execute("let rate: Information / Time = 3 Gigabyte / 1 Second")
            .unwrap();
        let completions = calculator.completion_names();
        assert!(completions.iter().any(|name| name == "Petabyte"));
        assert!(completions.iter().any(|name| name == "rate"));

        assert_eq!(
            calculator.execute("rate * 5 Hour as Terabyte").unwrap(),
            Some("54 Terabyte".into())
        );
        assert_eq!(
            calculator.execute("rate * 5 Hour").unwrap(),
            Some("54000 Gigabyte".into())
        );
    }

    #[test]
    fn builtin_units_need_no_imports_and_temperatures_convert_affinely() {
        let directory = tempdir().unwrap();
        let mut calculator = CalculatorSession::new(directory.path());

        // The exact affine transform, both directions.
        assert_eq!(
            calculator.execute("42 Celsius as Fahrenheit").unwrap(),
            Some("107.6 Fahrenheit".into())
        );
        assert_eq!(
            calculator.execute("32 Fahrenheit as Kelvin").unwrap(),
            Some("273.15 Kelvin".into())
        );
        assert_eq!(
            calculator.execute("212 Fahrenheit as Celsius").unwrap(),
            Some("100 Celsius".into())
        );

        // A difference of two points is a magnitude, reported as one.
        assert_eq!(
            calculator.execute("25 Celsius - 20 Celsius").unwrap(),
            Some("5 Kelvin".into())
        );
        assert_eq!(
            calculator.execute("100 Celsius - 32 Fahrenheit").unwrap(),
            Some("100 Kelvin".into())
        );
        // A point offset by a magnitude stays a point.
        assert_eq!(
            calculator.execute("20 Celsius + 5 Kelvin").unwrap(),
            Some("25 Celsius".into())
        );
        assert!(
            calculator
                .execute("20 Celsius + 5 Celsius")
                .unwrap_err()
                .to_string()
                .contains("E0837")
        );

        // Built-in units across the other families, without any import.
        for (input, expected) in [
            ("1 Mile as Foot", "5280 Foot"),
            ("1 Gigabit as Byte", "125000000 Byte"),
            ("1 Kibibyte as Byte", "1024 Byte"),
            ("1 Gallon as Liter", "3.785411784 Liter"),
            ("1 Year as Day", "365.25 Day"),
            ("1 BTU as Joule", "1055.05585262 Joule"),
            ("1 KilowattHour as Joule", "3600000 Joule"),
            // Pressure has a two-factor denominator, which the inferred
            // neutral type must parenthesize to keep the exponents intact.
            ("1 Newton / 1 Meter^2 as Pascal", "1 Pascal"),
            (
                "(100 Meter * 10 Millimeter) as Millimeter^2",
                "1000000 Millimeter^2",
            ),
            ("2 Meter^2 as Meter^2", "2 Meter^2"),
            ("(2 Meter)^2 as Meter^2", "4 Meter^2"),
            ("130 Millimeter^3 as Meter^3", "0.00000013 Meter^3"),
            (
                "1 Mile / 1 Hour as (Meter / Second)",
                "0.44704 Meter / Second",
            ),
            ("1 Kilowatt as Watt", "1000 Watt"),
        ] {
            assert_eq!(
                calculator.execute(input).unwrap(),
                Some(expected.to_owned()),
                "{input}"
            );
        }
    }

    #[test]
    fn incompatible_display_unit_is_an_error() {
        let directory = tempdir().unwrap();
        let mut calculator = CalculatorSession::new(directory.path());
        let error = calculator.execute("1 Meter as Second").unwrap_err();
        assert!(error.to_string().contains("incompatible"));
    }

    #[test]
    fn invalid_binding_does_not_enter_the_session() {
        let directory = tempdir().unwrap();
        let mut calculator = CalculatorSession::new(directory.path());
        assert!(calculator.execute("let broken = Missing").is_err());
        assert!(calculator.bindings().is_empty());
    }
}
