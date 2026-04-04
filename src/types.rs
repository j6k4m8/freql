use std::collections::BTreeMap;
use std::fmt;

use indexmap::IndexMap;
use num_bigint::BigInt;
use num_rational::BigRational;
use num_traits::{One, ToPrimitive, Zero};
use serde::Serialize;

use crate::ast::{ArraySize, BinaryOp, Expr, ExprKind, TypeArgument, TypeExpr, UnaryOp};

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize)]
pub struct Dimension(pub BTreeMap<String, BigRational>);

impl Dimension {
    pub fn scalar() -> Self {
        Self(BTreeMap::new())
    }

    pub fn base(name: &str) -> Self {
        Self(BTreeMap::from([(name.to_owned(), BigRational::one())]))
    }

    pub fn multiply(&self, other: &Self) -> Self {
        let mut result = self.0.clone();
        for (name, exponent) in &other.0 {
            *result.entry(name.clone()).or_insert_with(BigRational::zero) += exponent;
        }
        result.retain(|_, exponent| !exponent.is_zero());
        Self(result)
    }

    pub fn divide(&self, other: &Self) -> Self {
        let mut result = self.0.clone();
        for (name, exponent) in &other.0 {
            *result.entry(name.clone()).or_insert_with(BigRational::zero) -= exponent;
        }
        result.retain(|_, exponent| !exponent.is_zero());
        Self(result)
    }

    pub fn pow(&self, exponent: &BigRational) -> Self {
        Self(
            self.0
                .iter()
                .map(|(name, value)| (name.clone(), value * exponent))
                .filter(|(_, exponent)| !exponent.is_zero())
                .collect(),
        )
    }

    pub fn is_scalar(&self) -> bool {
        self.0.is_empty()
    }
}

impl fmt::Display for Dimension {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.0.is_empty() {
            return write!(f, "Scalar");
        }
        for (index, (name, exponent)) in self.0.iter().enumerate() {
            if index > 0 {
                write!(f, "·")?;
            }
            if exponent == &BigRational::one() {
                write!(f, "{name}")?;
            } else if exponent.denom() == &BigInt::one() {
                write!(f, "{name}^{}", exponent.numer())?;
            } else {
                write!(f, "{name}^({}/{})", exponent.numer(), exponent.denom())?;
            }
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Unit {
    pub name: String,
    pub dimension: Dimension,
    #[serde(with = "rational_string")]
    pub scale: BigRational,
    /// Additive term of the affine conversion `canonical = value * scale +
    /// offset`. Zero for every ordinary (purely multiplicative) unit; nonzero
    /// only for absolute temperature scales such as `Celsius`.
    #[serde(with = "rational_string")]
    pub offset: BigRational,
    /// How this unit is spelled in terms of *other* units, as a map from unit
    /// name to exponent (`Gbps` is `{Gigabit: 1, Second: -1}`).
    ///
    /// Empty means atomic: the unit spells itself. Presentation walks this to
    /// learn which concrete unit an author prefers for each base dimension —
    /// without it a composite contributes no spelling at all, and a product
    /// like `Gbps * Hour` cannot be reduced.
    #[serde(skip)]
    pub composition: BTreeMap<String, i32>,
    /// Whether this name denotes a *dimension* (`Length`, `Energy`, a user's
    /// `unit Information = base`) rather than a concrete measuring unit
    /// (`Meter`, `Joule`, `Byte`).
    ///
    /// Annotating a declaration with a concrete unit is a presentation choice
    /// worth preserving; annotating it with a dimension name is not. See
    /// [`inline_type_uses_concrete_unit`].
    #[serde(skip)]
    pub is_base_dimension: bool,
}

impl Unit {
    /// An affine *point* unit: its zero is not the dimension's zero, so it
    /// measures a position on a scale rather than a magnitude.
    pub fn is_absolute(&self) -> bool {
        !self.offset.is_zero()
    }

    /// Exact canonical value of `value` expressed in this unit.
    pub fn to_canonical(&self, value: &BigRational) -> BigRational {
        value * &self.scale + &self.offset
    }

    /// Exact value of a canonical quantity expressed in this unit.
    pub fn from_canonical(&self, canonical: &BigRational) -> BigRational {
        (canonical - &self.offset) / &self.scale
    }
}

/// A name is a dimension rather than a concrete unit when it is the identity
/// unit of a base dimension it names itself. This is what makes a user's
/// `unit Information = base` behave like the built-in `Length`.
fn names_own_base_dimension(name: &str, dimension: &Dimension, scale: &BigRational) -> bool {
    scale.is_one() && dimension.0.len() == 1 && dimension.0.get(name) == Some(&BigRational::one())
}

mod rational_string {
    use num_rational::BigRational;
    use serde::Serializer;

    pub fn serialize<S>(value: &BigRational, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&format!("{}/{}", value.numer(), value.denom()))
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
pub enum Type {
    /// A diagnostic has already been reported for whatever produced this type.
    ///
    /// [`compatible`] accepts it against anything so a single root cause does
    /// not cascade into a wave of follow-on errors. Produce this **only**
    /// after pushing a diagnostic, or when propagating a value that already
    /// carries one; producing it on a silent path is how type errors get
    /// laundered.
    ///
    /// Serialized as `unknown` because that is the spelling `freql inspect`
    /// has always emitted for an unresolved type.
    #[serde(rename = "unknown")]
    Error,
    /// No type could be determined and nothing has been reported yet.
    ///
    /// Unlike [`Type::Error`] this is **not** permissive in [`compatible`]:
    /// whoever consumes it is expected to report it. Callers that report it
    /// should then substitute [`Type::Error`] for the stored type so the
    /// failure does not cascade.
    Unknown,
    Bool,
    Integer,
    Scalar,
    /// A text literal. Deliberately incompatible with every numeric and
    /// quantity type: a string can never satisfy a dimensional annotation.
    String,
    Quantity {
        dimension: Dimension,
        display: String,
        /// Exact scale of one displayed unit in canonical dimension units.
        /// Kept internal because it is presentation/rounding metadata rather
        /// than part of the public semantic type identity.
        #[serde(skip)]
        scale: Option<BigRational>,
        /// An inline concrete unit expression in a declaration is an
        /// intentional presentation choice, unlike a unit inferred from a
        /// value expression. For example, `Mile / Hour` should display in
        /// miles per hour even if the assigned expression uses seconds.
        #[serde(skip)]
        explicit_presentation: bool,
        /// Whether this quantity is an affine *point* (`20 Celsius`) rather
        /// than a *vector* magnitude (`20 Kelvin`, `5 Meter`). Points may only
        /// be differenced, offset by a magnitude, and compared; they may not
        /// be added, scaled, negated, or composed into other units.
        #[serde(skip)]
        absolute: bool,
    },
    Artifact(String),
    Resource {
        name: String,
        capacity: Box<Type>,
        claim: Option<String>,
    },
    Array {
        element: Box<Type>,
        size: Option<String>,
    },
    Stream(Box<Type>),
    NamedTuple(Vec<(String, Type)>),
    Function,
    ConfiguredProcess(String),
    Run(String),
    TypeParameter(String),
}

impl Type {
    pub fn is_owned(&self) -> bool {
        match self {
            Self::Artifact(_) | Self::Stream(_) => true,
            Self::Array { element, .. } => element.is_owned(),
            Self::NamedTuple(fields) => fields.iter().any(|(_, ty)| ty.is_owned()),
            _ => false,
        }
    }

    pub fn is_copyable(&self) -> bool {
        match self {
            Self::NamedTuple(fields) => return fields.iter().all(|(_, ty)| ty.is_copyable()),
            Self::Array { element, .. } => return element.is_copyable(),
            _ => {}
        }
        !self.is_owned()
            && !matches!(
                self,
                Self::ConfiguredProcess(_) | Self::Run(_) | Self::Resource { .. }
            )
    }

    pub fn dimension(&self) -> Option<Dimension> {
        match self {
            Self::Integer | Self::Scalar => Some(Dimension::scalar()),
            Self::Quantity { dimension, .. } => Some(dimension.clone()),
            _ => None,
        }
    }

    pub fn unit_scale(&self) -> Option<BigRational> {
        match self {
            Self::Integer | Self::Scalar => Some(BigRational::one()),
            Self::Quantity { scale, .. } => scale.clone(),
            _ => None,
        }
    }

    /// Whether this is an affine point quantity such as `20 Celsius`. See
    /// [`Type::Quantity::absolute`].
    pub fn is_absolute(&self) -> bool {
        matches!(self, Self::Quantity { absolute: true, .. })
    }

    /// The name of the concrete unit a quantity displays in, when that unit is
    /// a single registered name rather than a compound expression.
    pub fn display_unit_name(&self) -> Option<&str> {
        match self {
            Self::Quantity { display, .. } => Some(display.as_str()),
            _ => None,
        }
    }
}

impl fmt::Display for Type {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            // Both unresolved cases read the same to an author; the split is
            // about whether a diagnostic has already been reported.
            Self::Error | Self::Unknown => write!(f, "Unknown"),
            Self::Bool => write!(f, "Bool"),
            Self::Integer => write!(f, "Integer"),
            Self::Scalar => write!(f, "Scalar"),
            Self::String => write!(f, "String"),
            Self::Quantity { display, .. } => write!(f, "{display}"),
            Self::Artifact(name) => write!(f, "{name}"),
            Self::Resource { name, claim, .. } => match claim {
                Some(claim) => write!(f, "{name}[{claim}]"),
                None => write!(f, "{name}"),
            },
            Self::Array { element, size } => match size {
                Some(size) => write!(f, "{element}[{size}]"),
                None => write!(f, "{element}[]"),
            },
            Self::Stream(element) => write!(f, "Stream[{element}]"),
            Self::NamedTuple(fields) => write!(
                f,
                "({})",
                fields
                    .iter()
                    .map(|(name, ty)| format!("{name}: {ty}"))
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            Self::Function => write!(f, "Function"),
            Self::ConfiguredProcess(name) => write!(f, "Configured[{name}]"),
            Self::Run(name) => write!(f, "Run[{name}]"),
            Self::TypeParameter(name) => write!(f, "{name}"),
        }
    }
}

#[derive(Clone, Debug)]
pub struct TypeRegistry {
    pub units: IndexMap<String, Unit>,
    pub resources: IndexMap<String, Type>,
    /// Refinements retain numeric representation; predicates are elaborated
    /// separately as requirements at typed boundaries.
    pub refinements: IndexMap<String, NumericRefinement>,
}

#[derive(Clone, Debug)]
pub struct NumericRefinement {
    pub base: Type,
    pub binder: String,
    pub predicates: Vec<Expr>,
}

/// Exact rational from a numerator and denominator, for readability at the
/// registration sites below.
fn ratio(numerator: i128, denominator: i128) -> BigRational {
    BigRational::new(numerator.into(), denominator.into())
}

fn integer(value: i128) -> BigRational {
    BigRational::from_integer(value.into())
}

/// Ten raised to a non-negative power, as an exact rational.
fn ten_pow(exponent: u32) -> BigRational {
    BigRational::from_integer(BigInt::from(10_u8).pow(exponent))
}

impl Default for TypeRegistry {
    fn default() -> Self {
        let mut registry = Self {
            units: IndexMap::new(),
            resources: IndexMap::new(),
            refinements: IndexMap::new(),
        };
        registry.register_builtins();
        registry
    }
}

impl TypeRegistry {
    /// The registry's preferred concrete unit for one base dimension symbol:
    /// the coherent unit of scale one (`Meter` for `L`, `Second` for `T`).
    /// Dimension *names* (`Length`) are skipped -- they are not spellings a
    /// magnitude should be rendered in.
    pub fn canonical_base_unit(&self, base: &str) -> Option<&Unit> {
        self.units.values().find(|unit| {
            !unit.is_base_dimension
                && unit.scale.is_one()
                && unit.offset.is_zero()
                && unit.dimension.0.len() == 1
                && unit.dimension.0.get(base) == Some(&BigRational::one())
        })
    }

    /// The standard named unit for a derived dimension, used to collapse
    /// `Kilogram * Meter^2 / Second^2` back to `Joule`.
    ///
    /// Curated rather than searched, because several derived units share a
    /// dimension and only one is the right default: `Becquerel` and `Hertz` are
    /// both `T^-1`, `Sievert` and `Gray` are both `L^2 * T^-2`, and `Lumen`
    /// shares `Candela`'s dimension entirely. Only the first of each pair is
    /// listed, and `Lumen` is omitted so plain luminous intensities are not
    /// relabelled.
    pub fn canonical_derived_unit(&self, dimension: &Dimension) -> Option<&Unit> {
        const CANONICAL_DERIVED: &[&str] = &[
            "Hertz", "Newton", "Pascal", "Joule", "Watt", "Coulomb", "Volt", "Ohm", "Siemens",
            "Farad", "Weber", "Tesla", "Henry", "Lux", "Gray", "Katal",
        ];
        CANONICAL_DERIVED
            .iter()
            .filter_map(|name| self.units.get(*name))
            .find(|unit| {
                unit.dimension == *dimension && unit.scale.is_one() && unit.offset.is_zero()
            })
    }

    pub fn define_resource(&mut self, name: &str, capacity: Type) {
        self.resources.insert(name.to_owned(), capacity);
    }

    /// A concrete measuring unit: `canonical = value * scale`.
    fn add_builtin(&mut self, name: &str, dimension: Dimension, scale: BigRational) {
        let is_base_dimension = names_own_base_dimension(name, &dimension, &scale);
        self.insert_unit(
            name,
            dimension,
            scale,
            BigRational::zero(),
            is_base_dimension,
        );
    }

    /// A name for a *dimension* rather than a unit (`Length`, `Energy`). Always
    /// canonical scale one; never a presentation choice.
    fn add_dimension(&mut self, name: &str, dimension: Dimension) {
        self.insert_unit(
            name,
            dimension,
            BigRational::one(),
            BigRational::zero(),
            true,
        );
    }

    /// An affine point unit: `canonical = value * scale + offset`.
    fn add_affine(
        &mut self,
        name: &str,
        dimension: Dimension,
        scale: BigRational,
        offset: BigRational,
    ) {
        self.insert_unit(name, dimension, scale, offset, false);
    }

    fn insert_unit(
        &mut self,
        name: &str,
        dimension: Dimension,
        scale: BigRational,
        offset: BigRational,
        is_base_dimension: bool,
    ) {
        self.insert_composed_unit(
            name,
            dimension,
            scale,
            offset,
            is_base_dimension,
            BTreeMap::new(),
        );
    }

    fn insert_composed_unit(
        &mut self,
        name: &str,
        dimension: Dimension,
        scale: BigRational,
        offset: BigRational,
        is_base_dimension: bool,
        composition: BTreeMap<String, i32>,
    ) {
        self.units.insert(
            name.into(),
            Unit {
                name: name.into(),
                dimension,
                scale,
                offset,
                composition,
                is_base_dimension,
            },
        );
    }

    /// Register a derived unit spelled in terms of other registered units, so
    /// presentation can recover the author's preferred base spellings from it.
    fn add_composed(&mut self, name: &str, dimension: Dimension, composition: &[(&str, i32)]) {
        self.add_scaled_composed(name, dimension, BigRational::one(), composition);
    }

    fn add_scaled_composed(
        &mut self,
        name: &str,
        dimension: Dimension,
        scale: BigRational,
        composition: &[(&str, i32)],
    ) {
        let composition = composition
            .iter()
            .map(|(unit, power)| ((*unit).to_owned(), *power))
            .collect();
        self.insert_composed_unit(
            name,
            dimension,
            scale,
            BigRational::zero(),
            false,
            composition,
        );
    }

    #[expect(
        clippy::too_many_lines,
        reason = "a flat, auditable table of unit definitions reads better in \
                  one place than split across arbitrary helper boundaries"
    )]
    fn register_builtins(&mut self) {
        let scalar = Dimension::scalar();
        let length = Dimension::base("L");
        let mass = Dimension::base("M");
        let time = Dimension::base("T");
        let current = Dimension::base("I");
        let temperature = Dimension::base("Θ");
        let amount = Dimension::base("N");
        let luminous = Dimension::base("J");
        let information = Dimension::base("Information");
        let money = Dimension::base("USD");
        // Compose base dimensions by exponent, e.g. `dim(&[(&length, 2), (&time, -2)])`.
        let dim = |factors: &[(&Dimension, i32)]| {
            factors
                .iter()
                .fold(Dimension::scalar(), |accumulated, (base, exponent)| {
                    accumulated.multiply(&base.pow(&integer((*exponent).into())))
                })
        };

        let area = dim(&[(&length, 2)]);
        let volume = dim(&[(&length, 3)]);
        let frequency = dim(&[(&time, -1)]);
        let velocity = dim(&[(&length, 1), (&time, -1)]);
        let acceleration = dim(&[(&length, 1), (&time, -2)]);
        let force = dim(&[(&length, 1), (&mass, 1), (&time, -2)]);
        let pressure = dim(&[(&length, -1), (&mass, 1), (&time, -2)]);
        let energy = dim(&[(&length, 2), (&mass, 1), (&time, -2)]);
        let power = dim(&[(&length, 2), (&mass, 1), (&time, -3)]);
        let charge = dim(&[(&time, 1), (&current, 1)]);
        let voltage = dim(&[(&length, 2), (&mass, 1), (&time, -3), (&current, -1)]);
        let resistance = dim(&[(&length, 2), (&mass, 1), (&time, -3), (&current, -2)]);
        let conductance = dim(&[(&length, -2), (&mass, -1), (&time, 3), (&current, 2)]);
        let capacitance = dim(&[(&length, -2), (&mass, -1), (&time, 4), (&current, 2)]);
        let magnetic_flux = dim(&[(&length, 2), (&mass, 1), (&time, -2), (&current, -1)]);
        let flux_density = dim(&[(&mass, 1), (&time, -2), (&current, -1)]);
        let inductance = dim(&[(&length, 2), (&mass, 1), (&time, -2), (&current, -2)]);
        let illuminance = dim(&[(&luminous, 1), (&length, -2)]);
        let dose = dim(&[(&length, 2), (&time, -2)]);
        let catalytic = dim(&[(&amount, 1), (&time, -1)]);
        let density = dim(&[(&mass, 1), (&length, -3)]);
        let momentum = dim(&[(&length, 1), (&mass, 1), (&time, -1)]);
        let mass_flow = dim(&[(&mass, 1), (&time, -1)]);
        let volume_flow = dim(&[(&length, 3), (&time, -1)]);
        let information_rate = dim(&[(&information, 1), (&time, -1)]);

        // --- SI base dimensions and their base units -----------------------
        self.add_dimension("Scalar", scalar.clone());
        self.add_dimension("Length", length.clone());
        self.add_builtin("Meter", length.clone(), BigRational::one());
        self.add_dimension("Mass", mass.clone());
        self.add_builtin("Kilogram", mass.clone(), BigRational::one());
        self.add_dimension("Time", time.clone());
        self.add_builtin("Second", time.clone(), BigRational::one());
        self.add_dimension("Current", current.clone());
        self.add_builtin("Ampere", current.clone(), BigRational::one());
        self.add_dimension("Temperature", temperature.clone());
        self.add_dimension("Amount", amount.clone());
        self.add_builtin("Mole", amount.clone(), BigRational::one());
        self.add_dimension("LuminousIntensity", luminous.clone());
        self.add_builtin("Candela", luminous.clone(), BigRational::one());
        // Currency is its own base dimension. No other currency is registered:
        // exchange rates are not fixed conversion factors, so a built-in
        // `unit EUR = USD * k` would bake a lie into the type system.
        self.add_builtin("USD", money, BigRational::one());

        // --- Geometry ------------------------------------------------------
        self.add_dimension("Area", area);
        self.add_dimension("Volume", volume.clone());

        // --- SI derived units, with dimension-name aliases -----------------
        self.add_dimension("Frequency", frequency.clone());
        self.add_composed("Hertz", frequency.clone(), &[("Second", -1)]);
        self.add_dimension("Force", force.clone());
        self.add_composed(
            "Newton",
            force.clone(),
            &[("Kilogram", 1), ("Meter", 1), ("Second", -2)],
        );
        self.add_dimension("Pressure", pressure.clone());
        self.add_composed("Pascal", pressure.clone(), &[("Newton", 1), ("Meter", -2)]);
        self.add_dimension("Energy", energy.clone());
        self.add_composed("Joule", energy.clone(), &[("Newton", 1), ("Meter", 1)]);
        self.add_dimension("Power", power.clone());
        self.add_composed("Watt", power.clone(), &[("Joule", 1), ("Second", -1)]);
        self.add_dimension("Charge", charge.clone());
        self.add_composed("Coulomb", charge, &[("Ampere", 1), ("Second", 1)]);
        self.add_dimension("Voltage", voltage.clone());
        self.add_composed("Volt", voltage.clone(), &[("Watt", 1), ("Ampere", -1)]);
        self.add_dimension("Resistance", resistance.clone());
        self.add_composed("Ohm", resistance, &[("Volt", 1), ("Ampere", -1)]);
        self.add_dimension("Conductance", conductance.clone());
        self.add_composed("Siemens", conductance, &[("Ampere", 1), ("Volt", -1)]);
        self.add_dimension("Capacitance", capacitance.clone());
        self.add_composed("Farad", capacitance, &[("Coulomb", 1), ("Volt", -1)]);
        self.add_dimension("MagneticFlux", magnetic_flux.clone());
        self.add_composed("Weber", magnetic_flux, &[("Volt", 1), ("Second", 1)]);
        self.add_dimension("MagneticFluxDensity", flux_density.clone());
        self.add_composed("Tesla", flux_density, &[("Weber", 1), ("Meter", -2)]);
        self.add_dimension("Inductance", inductance.clone());
        self.add_composed("Henry", inductance, &[("Weber", 1), ("Ampere", -1)]);
        // Luminous flux is candela·steradian; the steradian is dimensionless,
        // so lumen shares luminous intensity's dimension.
        self.add_dimension("LuminousFlux", luminous.clone());
        self.add_builtin("Lumen", luminous.clone(), BigRational::one());
        self.add_dimension("Illuminance", illuminance.clone());
        self.add_composed("Lux", illuminance, &[("Lumen", 1), ("Meter", -2)]);
        // Becquerel and hertz share the dimension T^-1 but are deliberately
        // separate names, as in SI.
        self.add_dimension("Radioactivity", frequency.clone());
        self.add_composed("Becquerel", frequency.clone(), &[("Second", -1)]);
        self.add_dimension("AbsorbedDose", dose.clone());
        self.add_composed("Gray", dose.clone(), &[("Joule", 1), ("Kilogram", -1)]);
        self.add_dimension("DoseEquivalent", dose.clone());
        self.add_composed("Sievert", dose, &[("Joule", 1), ("Kilogram", -1)]);
        self.add_dimension("CatalyticActivity", catalytic.clone());
        self.add_composed("Katal", catalytic, &[("Mole", 1), ("Second", -1)]);

        // --- Kinematic and mechanical dimensions ---------------------------
        self.add_dimension("Velocity", velocity.clone());
        self.add_dimension("Speed", velocity);
        self.add_dimension("Acceleration", acceleration);
        self.add_dimension("Density", density);
        self.add_dimension("Momentum", momentum);
        self.add_dimension("MassFlowRate", mass_flow);
        self.add_dimension("VolumetricFlowRate", volume_flow);

        // --- Calendar time -------------------------------------------------
        // `Month` is deliberately absent: calendar months are 28-31 days, so
        // no exact conversion factor to seconds exists. Models that need one
        // must state the convention themselves with `unit Month = Day * 30`.
        self.add_builtin("Minute", time.clone(), integer(60));
        self.add_builtin("Hour", time.clone(), integer(3_600));
        self.add_builtin("Day", time.clone(), integer(86_400));
        self.add_builtin("Week", time.clone(), integer(604_800));
        // The Julian year, 365.25 days, is the exact convention SI uses.
        self.add_builtin("Year", time.clone(), integer(31_557_600));

        // --- Information ---------------------------------------------------
        // Canonical size is the byte, so `Bit` is exactly 1/8.
        self.add_dimension("Information", information.clone());
        self.add_builtin("Byte", information.clone(), BigRational::one());
        self.add_builtin("Bit", information.clone(), ratio(1, 8));
        let decimal_bytes = [
            ("Kilobyte", 3),
            ("Megabyte", 6),
            ("Gigabyte", 9),
            ("Terabyte", 12),
            ("Petabyte", 15),
            ("Exabyte", 18),
        ];
        for (name, exponent) in decimal_bytes {
            self.add_builtin(name, information.clone(), ten_pow(exponent));
        }
        let decimal_bits = [
            ("Kilobit", 3),
            ("Megabit", 6),
            ("Gigabit", 9),
            ("Terabit", 12),
            ("Petabit", 15),
            ("Exabit", 18),
        ];
        for (name, exponent) in decimal_bits {
            self.add_builtin(name, information.clone(), ten_pow(exponent) / integer(8));
        }
        let binary_bytes = [
            ("Kibibyte", 1_024_i128),
            ("Mebibyte", 1_048_576),
            ("Gibibyte", 1_073_741_824),
            ("Tebibyte", 1_099_511_627_776),
        ];
        for (name, factor) in binary_bytes {
            self.add_builtin(name, information.clone(), integer(factor));
        }
        // Throughput units, matching the size units above.
        self.add_scaled_composed(
            "BytePerSecond",
            information_rate.clone(),
            BigRational::one(),
            &[("Byte", 1), ("Second", -1)],
        );
        self.add_scaled_composed(
            "BitPerSecond",
            information_rate.clone(),
            ratio(1, 8),
            &[("Bit", 1), ("Second", -1)],
        );
        for (name, exponent) in decimal_bytes {
            self.add_scaled_composed(
                &format!("{name}PerSecond"),
                information_rate.clone(),
                ten_pow(exponent),
                &[(name, 1), ("Second", -1)],
            );
        }
        for (name, exponent) in decimal_bits {
            self.add_scaled_composed(
                &format!("{name}PerSecond"),
                information_rate.clone(),
                ten_pow(exponent) / integer(8),
                &[(name, 1), ("Second", -1)],
            );
        }

        // --- US customary and imperial -------------------------------------
        // Everything below derives from the 1959 international agreement
        // (1 inch = 25.4 mm exactly, 1 pound = 0.45359237 kg exactly), so all
        // of these factors are exact rationals.
        let inch = ratio(127, 5_000);
        let foot = &inch * integer(12);
        let yard = &foot * integer(3);
        let mile = &yard * integer(1_760);
        self.add_builtin("Inch", length.clone(), inch.clone());
        self.add_builtin("Foot", length.clone(), foot.clone());
        self.add_builtin("Yard", length.clone(), yard.clone());
        self.add_builtin("Mile", length.clone(), mile);
        let pound = ratio(45_359_237, 100_000_000);
        self.add_builtin("Pound", mass.clone(), pound.clone());
        self.add_builtin("Ounce", mass.clone(), &pound / integer(16));
        // 1 US liquid gallon = 231 cubic inches, exactly.
        self.add_builtin(
            "Gallon",
            volume.clone(),
            pow_rational(inch.clone(), 3) * integer(231),
        );
        self.add_builtin("Liter", volume.clone(), ratio(1, 1_000));
        // Standard gravity is defined as exactly 9.80665 m/s^2, which makes
        // pound-force, psi and horsepower exact.
        let standard_gravity = ratio(980_665, 100_000);
        let pound_force = &pound * &standard_gravity;
        self.add_builtin(
            "PSI",
            pressure.clone(),
            &pound_force / pow_rational(inch, 2),
        );
        // The International Table BTU, 1055.05585262 J exactly.
        self.add_builtin("BTU", energy.clone(), ratio(105_505_585_262, 100_000_000));
        // Mechanical horsepower: 550 foot-pounds-force per second.
        self.add_builtin(
            "Horsepower",
            power.clone(),
            &pound_force * &foot * integer(550),
        );
        // 1 acre = 4840 square yards.
        self.add_builtin(
            "Acre",
            dim(&[(&length, 2)]),
            pow_rational(yard, 2) * integer(4_840),
        );

        // --- Angle and dimensionless ratios --------------------------------
        // `Degree` is deliberately absent: 1 degree is pi/180 radians and pi is
        // irrational, so no exact `BigRational` scale exists for it. Every
        // scale in this registry is exact, and silently substituting a
        // truncated decimal would make `x as Degree` quietly wrong. Angles are
        // expressed in radians; a model that wants degrees must opt into an
        // approximation explicitly with its own `unit` declaration.
        self.add_builtin("Radian", scalar.clone(), BigRational::one());
        self.add_builtin("Steradian", scalar.clone(), BigRational::one());
        self.add_builtin("Count", scalar.clone(), BigRational::one());
        self.add_builtin("Percent", scalar.clone(), ratio(1, 100));
        self.add_builtin("PPM", scalar, ratio(1, 1_000_000));

        // --- Temperature ---------------------------------------------------
        // Absolute (point) scales carry an offset; the corresponding relative
        // (interval) scales do not. `canonical = value * scale + offset`.
        self.add_builtin("Kelvin", temperature.clone(), BigRational::one());
        self.add_builtin("Rankine", temperature.clone(), ratio(5, 9));
        self.add_affine(
            "Celsius",
            temperature.clone(),
            BigRational::one(),
            // 273.15
            ratio(5_463, 20),
        );
        self.add_affine(
            "Fahrenheit",
            temperature.clone(),
            ratio(5, 9),
            // 273.15 - 160/9
            ratio(45_967, 180),
        );
        // Interval aliases, for readers who expect a degree-shaped name when
        // writing a temperature *difference*.
        self.add_builtin("CelsiusDegree", temperature.clone(), BigRational::one());
        self.add_builtin("FahrenheitDegree", temperature.clone(), ratio(5, 9));

        // --- Curated SI prefixes -------------------------------------------
        // Deliberately a curated list rather than a general prefix mechanism:
        // these are the spellings that actually show up in engineering models.
        // Anything else is one line of `unit X = ...` away.
        self.add_builtin("Millimeter", length.clone(), ratio(1, 1_000));
        self.add_builtin("Centimeter", length.clone(), ratio(1, 100));
        self.add_builtin("Kilometer", length.clone(), integer(1_000));
        self.add_builtin("Micrometer", length.clone(), ratio(1, 1_000_000));
        self.add_builtin("Nanometer", length.clone(), ratio(1, 1_000_000_000));
        self.add_builtin("Gram", mass.clone(), ratio(1, 1_000));
        self.add_builtin("Milligram", mass.clone(), ratio(1, 1_000_000));
        self.add_builtin("Microgram", mass.clone(), ratio(1, 1_000_000_000));
        self.add_builtin("Tonne", mass.clone(), integer(1_000));
        self.add_builtin("Millisecond", time.clone(), ratio(1, 1_000));
        self.add_builtin("Microsecond", time.clone(), ratio(1, 1_000_000));
        self.add_builtin("Nanosecond", time.clone(), ratio(1, 1_000_000_000));
        self.add_builtin("Kilowatt", power.clone(), integer(1_000));
        self.add_builtin("Megawatt", power.clone(), integer(1_000_000));
        self.add_builtin("Gigawatt", power.clone(), integer(1_000_000_000));
        self.add_builtin("Kilojoule", energy.clone(), integer(1_000));
        self.add_builtin("Megajoule", energy.clone(), integer(1_000_000));
        self.add_builtin("Kilonewton", force, integer(1_000));
        self.add_builtin("Kilopascal", pressure.clone(), integer(1_000));
        self.add_builtin("Megapascal", pressure, integer(1_000_000));
        self.add_builtin("Millivolt", voltage.clone(), ratio(1, 1_000));
        self.add_builtin("Kilovolt", voltage, integer(1_000));
        self.add_builtin("Milliampere", current, ratio(1, 1_000));
        self.add_builtin("KilowattHour", energy, integer(3_600_000));
    }

    pub fn resolve_type(
        &self,
        ty: &TypeExpr,
        artifacts: &IndexMap<String, String>,
        type_parameters: &[String],
    ) -> Type {
        let mut base = if let Some(fields) = &ty.fields {
            let fields = fields
                .iter()
                .map(|field| {
                    (
                        field.name.clone(),
                        self.resolve_type(&field.ty, artifacts, type_parameters),
                    )
                })
                .collect::<Vec<_>>();
            if fields
                .iter()
                .any(|(_, ty)| !ty.is_copyable() || !type_is_resolved(ty))
            {
                Type::Unknown
            } else {
                Type::NamedTuple(fields)
            }
        } else if let Some(dimension) = &ty.dimension {
            match self.eval_unit_expr(dimension) {
                Ok(value) if value.dimension.is_scalar() => Type::Scalar,
                Ok(value) => Type::Quantity {
                    dimension: value.dimension,
                    display: inline_unit_display(ty.dimension.as_ref().expect("inline unit type")),
                    scale: Some(value.scale),
                    explicit_presentation: inline_type_uses_concrete_unit(
                        ty.dimension.as_ref().expect("inline unit type"),
                        self,
                    ),
                    absolute: !value.offset.is_zero(),
                },
                Err(_) => Type::Unknown,
            }
        } else {
            match ty.name.as_str() {
                "Bool" => Type::Bool,
                "Integer" => Type::Integer,
                "Scalar" => Type::Scalar,
                "String" => Type::String,
                name if self.refinements.contains_key(name) => self.refinements[name].base.clone(),
                "Stream" => ty
                    .arguments
                    .first()
                    .and_then(|argument| match argument {
                        TypeArgument::Type(inner) => Some(Type::Stream(Box::new(
                            self.resolve_type(inner, artifacts, type_parameters),
                        ))),
                        TypeArgument::Const(_) => None,
                    })
                    .unwrap_or(Type::Unknown),
                "Configured" => ty
                    .arguments
                    .first()
                    .and_then(|argument| match argument {
                        TypeArgument::Type(process)
                            if process.dimension.is_none() && process.arguments.is_empty() =>
                        {
                            Some(Type::ConfiguredProcess(process.name.clone()))
                        }
                        TypeArgument::Type(_) | TypeArgument::Const(_) => None,
                    })
                    .unwrap_or(Type::Unknown),
                name if self.resources.contains_key(name) => Type::Resource {
                    name: name.to_owned(),
                    capacity: Box::new(self.resources[name].clone()),
                    claim: ty.arrays.first().and_then(|array| match array {
                        ArraySize::Fixed(value) => Some(value.clone()),
                        ArraySize::Dynamic => None,
                    }),
                },
                name if type_parameters.iter().any(|parameter| parameter == name) => {
                    Type::TypeParameter(name.into())
                }
                name if artifacts.contains_key(name) => Type::Artifact(name.into()),
                name => self
                    .units
                    .get(name)
                    .map(|unit| {
                        if unit.dimension.is_scalar() {
                            Type::Scalar
                        } else {
                            Type::Quantity {
                                dimension: unit.dimension.clone(),
                                display: name.into(),
                                scale: Some(unit.scale.clone()),
                                explicit_presentation: false,
                                absolute: unit.is_absolute(),
                            }
                        }
                    })
                    .unwrap_or(Type::Unknown),
            }
        };
        if matches!(base, Type::Resource { .. }) {
            return base;
        }
        for array in &ty.arrays {
            base = Type::Array {
                element: Box::new(base),
                size: match array {
                    ArraySize::Dynamic => None,
                    ArraySize::Fixed(size) => Some(size.clone()),
                },
            };
        }
        base
    }

    pub fn define_unit(&mut self, name: &str, expression: &Expr) -> Result<Unit, String> {
        let value = match &expression.kind {
            ExprKind::Name { value } if value == "base" => UnitValue {
                dimension: Dimension::base(name),
                scale: BigRational::one(),
                offset: BigRational::zero(),
            },
            _ => self.eval_unit_expr(expression)?,
        };
        let is_base_dimension = names_own_base_dimension(name, &value.dimension, &value.scale);
        // Keep the spelling the author actually wrote. `unit Gbps = Gigabit /
        // Second` is what tells presentation that this model thinks in
        // gigabits and seconds.
        let composition = unit_expression_factors(expression, self).unwrap_or_default();
        let unit = Unit {
            name: name.into(),
            dimension: value.dimension,
            scale: value.scale,
            offset: value.offset,
            composition,
            is_base_dimension,
        };
        // A plain `insert` is what lets a model's own `unit Hour = Second *
        // 3600` silently take precedence over the built-in of the same name.
        // Explicit user definitions are authoritative, so shadowing a
        // built-in does not produce a diagnostic.
        self.units.insert(name.into(), unit.clone());
        Ok(unit)
    }

    /// Resolve an author-selected presentation unit without declaring a new
    /// language-level unit. Named units retain their registered affine
    /// behavior; compound expressions are necessarily multiplicative and use
    /// the exact combined scale and dimension.
    pub fn resolve_display_unit(&self, label: &str, expression: &Expr) -> Result<Unit, String> {
        if let Some(unit) = self.units.get(label) {
            return Ok(unit.clone());
        }
        let value = self.eval_unit_expr(expression)?;
        Ok(Unit {
            name: label.to_owned(),
            dimension: value.dimension,
            scale: value.scale,
            offset: value.offset,
            composition: unit_expression_factors(expression, self).unwrap_or_default(),
            is_base_dimension: false,
        })
    }

    /// The preferred type-level name for each base dimension symbol in scope,
    /// e.g. `L -> Length`, `USD -> USD`, and a model's own `unit Credit = base`
    /// as `Credit -> Credit`.
    ///
    /// Derived from the registry so that adding a dimension in one place is
    /// enough; the first registered name for a symbol wins.
    pub fn base_dimension_names(&self) -> IndexMap<String, String> {
        let mut names = IndexMap::new();
        for unit in self.units.values() {
            if !unit.is_base_dimension {
                continue;
            }
            let mut factors = unit.dimension.0.iter();
            let (Some((symbol, exponent)), None) = (factors.next(), factors.next()) else {
                continue;
            };
            if !exponent.is_one() {
                continue;
            }
            names
                .entry(symbol.clone())
                .or_insert_with(|| unit.name.clone());
        }
        names
    }

    /// Look up the relative (interval) unit that measures differences on an
    /// absolute unit's scale: `Celsius`/`CelsiusDegree` -> `Kelvin`,
    /// `Fahrenheit` -> `Rankine`. Matching is by exact scale within the same
    /// dimension, so it keeps working for user-declared affine units.
    pub fn relative_counterpart(&self, unit: &Unit) -> Option<&Unit> {
        self.units.values().find(|candidate| {
            !candidate.is_absolute()
                && !candidate.is_base_dimension
                && candidate.dimension == unit.dimension
                && candidate.scale == unit.scale
        })
    }

    /// Why an inline unit annotation such as `Celsius / Second` failed to
    /// resolve, so that callers can report the actual reason instead of a bare
    /// "unknown type".
    pub fn inline_unit_error(&self, ty: &TypeExpr) -> Option<String> {
        self.eval_unit_expr(ty.dimension.as_ref()?).err()
    }

    fn eval_unit_expr(&self, expression: &Expr) -> Result<UnitValue, String> {
        match &expression.kind {
            ExprKind::Name { value } => self
                .units
                .get(value)
                .map(|unit| UnitValue {
                    dimension: unit.dimension.clone(),
                    scale: unit.scale.clone(),
                    offset: unit.offset.clone(),
                })
                .ok_or_else(|| format!("unknown unit or dimension {value}")),
            ExprKind::Number {
                value,
                suffix: None,
            } => Ok(UnitValue {
                dimension: Dimension::scalar(),
                scale: parse_rational(value)
                    .ok_or_else(|| format!("invalid exact number {value}"))?,
                offset: BigRational::zero(),
            }),
            ExprKind::Binary { op, left, right } if matches!(op, BinaryOp::Mul | BinaryOp::Div) => {
                let left = self.eval_unit_expr(left)?;
                let right = self.eval_unit_expr(right)?;
                self.reject_absolute_operand(&left)?;
                self.reject_absolute_operand(&right)?;
                Ok(if *op == BinaryOp::Mul {
                    UnitValue {
                        dimension: left.dimension.multiply(&right.dimension),
                        scale: left.scale * right.scale,
                        offset: BigRational::zero(),
                    }
                } else {
                    UnitValue {
                        dimension: left.dimension.divide(&right.dimension),
                        scale: left.scale / right.scale,
                        offset: BigRational::zero(),
                    }
                })
            }
            ExprKind::Binary {
                op: BinaryOp::Pow,
                left,
                right,
            } => {
                let base = self.eval_unit_expr(left)?;
                self.reject_absolute_operand(&base)?;
                let exponent = signed_scalar_literal(right)
                    .filter(|value| value.is_integer())
                    .and_then(|value| value.to_integer().to_i32())
                    .ok_or_else(|| {
                        "unit powers must be signed integer literals in the i32 range".to_owned()
                    })?;
                if base.scale.is_zero() && exponent < 0 {
                    return Err("zero cannot be raised to a negative power".into());
                }
                Ok(UnitValue {
                    dimension: base
                        .dimension
                        .pow(&BigRational::from_integer(exponent.into())),
                    scale: pow_rational(base.scale, exponent),
                    offset: BigRational::zero(),
                })
            }
            _ => Err(format!(
                "unsupported unit definition expression {}",
                expression.display()
            )),
        }
    }

    /// An affine unit has no meaning as a factor: `Celsius / Second` would
    /// have to answer "0 °C per second is how many K/s?", and there is no
    /// consistent answer. Compound units must be built from interval units.
    fn reject_absolute_operand(&self, value: &UnitValue) -> Result<(), String> {
        if value.offset.is_zero() {
            return Ok(());
        }
        let offender = self.units.values().find(|unit| {
            unit.dimension == value.dimension
                && unit.scale == value.scale
                && unit.offset == value.offset
        });
        let (name, suggestion) = offender.map_or_else(
            || ("with a scale offset", "a relative unit"),
            |unit| {
                (
                    unit.name.as_str(),
                    self.relative_counterpart(unit)
                        .map_or("a relative unit", |relative| relative.name.as_str()),
                )
            },
        );
        Err(format!(
            "absolute unit {name} cannot appear in a compound unit; use {suggestion} for \
             temperature rates and per-degree quantities"
        ))
    }
}

/// The exact affine conversion a unit expression denotes:
/// `canonical = value * scale + offset`.
#[derive(Clone, Debug)]
struct UnitValue {
    dimension: Dimension,
    scale: BigRational,
    offset: BigRational,
}

/// Decompose a unit definition into the unit names it is spelled with.
///
/// Mirrors `eval_unit_expr`'s shape but keeps names instead of collapsing to a
/// dimension, so `Gigabit / Second` yields `{Gigabit: 1, Second: -1}`. Numeric
/// scale factors carry no spelling and are skipped: the scale is already
/// recorded separately.
fn unit_expression_factors(
    expression: &Expr,
    registry: &TypeRegistry,
) -> Option<BTreeMap<String, i32>> {
    match &expression.kind {
        ExprKind::Name { value } if registry.units.contains_key(value) => {
            Some(BTreeMap::from([(value.clone(), 1)]))
        }
        ExprKind::Number { .. } => Some(BTreeMap::new()),
        ExprKind::Binary { op, left, right } if matches!(op, BinaryOp::Mul | BinaryOp::Div) => {
            let left = unit_expression_factors(left, registry)?;
            let right = unit_expression_factors(right, registry)?;
            let sign = if *op == BinaryOp::Mul { 1 } else { -1 };
            let mut combined = left;
            for (unit, power) in right {
                *combined.entry(unit).or_default() += power * sign;
            }
            combined.retain(|_, power| *power != 0);
            Some(combined)
        }
        ExprKind::Binary {
            op: BinaryOp::Pow,
            left,
            right,
        } => {
            let exponent = signed_scalar_literal(right)?;
            if !exponent.is_integer() {
                return None;
            }
            let exponent = exponent.to_integer().to_i32()?;
            let factors = unit_expression_factors(left, registry)?;
            factors
                .into_iter()
                .map(|(unit, power)| power.checked_mul(exponent).map(|power| (unit, power)))
                .collect()
        }
        _ => None,
    }
}

fn inline_unit_display(expression: &Expr) -> String {
    let display = expression.display();
    display
        .strip_prefix('(')
        .and_then(|display| display.strip_suffix(')'))
        .unwrap_or(&display)
        .to_owned()
}

/// Whether an inline unit annotation names a concrete measuring unit — a
/// presentation choice worth preserving — rather than a bare dimension.
///
/// Registry-derived rather than a hardcoded name list: with roughly a hundred
/// built-in units, deciding by *absence* from a list would silently classify
/// every newly registered dimension name as a concrete unit.
fn inline_type_uses_concrete_unit(expression: &Expr, registry: &TypeRegistry) -> bool {
    match &expression.kind {
        ExprKind::Name { value } => !registry
            .units
            .get(value)
            .is_some_and(|unit| unit.is_base_dimension),
        ExprKind::Number { .. } => false,
        ExprKind::Unary { value, .. } => inline_type_uses_concrete_unit(value, registry),
        ExprKind::Binary { left, right, .. } => {
            inline_type_uses_concrete_unit(left, registry)
                || inline_type_uses_concrete_unit(right, registry)
        }
        _ => false,
    }
}

fn type_is_resolved(ty: &Type) -> bool {
    match ty {
        Type::Unknown => false,
        Type::Array { element, .. } | Type::Stream(element) => type_is_resolved(element),
        Type::NamedTuple(fields) => fields.iter().all(|(_, field)| type_is_resolved(field)),
        _ => true,
    }
}

pub fn compatible(left: &Type, right: &Type) -> bool {
    // `Type::Error` already produced a diagnostic, so accepting it everywhere
    // suppresses the follow-on noise. `Type::Unknown` is deliberately *not*
    // listed here: it means "nothing determined and nothing reported", and it
    // must be rejected so the mismatch surfaces.
    if left == &Type::Error || right == &Type::Error {
        return true;
    }
    match (left, right) {
        (Type::Integer, Type::Scalar) | (Type::Scalar, Type::Integer) => true,
        (
            Type::Quantity {
                dimension: left, ..
            },
            Type::Quantity {
                dimension: right, ..
            },
        ) => left == right,
        (
            Type::Array {
                element: left,
                size: left_size,
            },
            Type::Array {
                element: right,
                size: right_size,
            },
        ) => {
            compatible(left, right)
                && match (left_size, right_size) {
                    (Some(left), Some(right)) => left == right,
                    _ => true,
                }
        }
        (Type::Stream(left), Type::Stream(right)) => compatible(left, right),
        (Type::NamedTuple(left), Type::NamedTuple(right)) => {
            left.len() == right.len()
                && left.iter().all(|(name, ty)| {
                    right
                        .iter()
                        .find(|(other, _)| other == name)
                        .is_some_and(|(_, other)| compatible(ty, other))
                })
        }
        (
            Type::Resource {
                name: left_name,
                capacity: left_capacity,
                ..
            },
            Type::Resource {
                name: right_name,
                capacity: right_capacity,
                ..
            },
        ) => left_name == right_name && compatible(left_capacity, right_capacity),
        _ => left == right,
    }
}

/// Signs are unary AST nodes. Accept signed literals without accidentally
/// treating a dimensioned exponent such as `-1 Gigabyte` as a scalar.
pub(crate) fn signed_scalar_literal(expression: &Expr) -> Option<BigRational> {
    match &expression.kind {
        ExprKind::Number {
            value,
            suffix: None,
        } => parse_rational(value),
        ExprKind::Unary {
            op: UnaryOp::Neg,
            value,
        } => signed_scalar_literal(value).map(|v| -v),
        ExprKind::Unary {
            op: UnaryOp::Pos,
            value,
        } => signed_scalar_literal(value),
        _ => None,
    }
}

pub fn parse_rational(value: &str) -> Option<BigRational> {
    if let Some((mantissa, exponent)) = value.split_once(['e', 'E']) {
        let mut rational = parse_rational(mantissa)?;
        let exponent = exponent.parse::<i32>().ok()?;
        let ten = BigRational::from_integer(10.into());
        rational *= pow_rational(ten, exponent);
        return Some(rational);
    }
    if let Some((whole, fraction)) = value.split_once('.') {
        let numerator = format!("{whole}{fraction}").parse::<BigInt>().ok()?;
        let denominator = BigInt::from(10_u8).pow(fraction.len() as u32);
        Some(BigRational::new(numerator, denominator))
    } else {
        value.parse::<BigInt>().ok().map(BigRational::from_integer)
    }
}

fn pow_rational(mut value: BigRational, exponent: i32) -> BigRational {
    if exponent == 0 {
        return BigRational::one();
    }
    let invert = exponent < 0;
    let mut exponent = exponent.unsigned_abs();
    let mut result = BigRational::one();
    while exponent > 0 {
        if exponent % 2 == 1 {
            result *= value.clone();
        }
        value = value.clone() * value;
        exponent /= 2;
    }
    if invert { result.recip() } else { result }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::parse;

    #[test]
    fn derives_scaled_and_structural_units() {
        let parsed = parse("unit Millimeter = Meter / 1000 unit Density = Mass / Volume");
        assert!(parsed.errors.is_empty(), "{:#?}", parsed.errors);
        let mut registry = TypeRegistry::default();
        for item in parsed.module.items {
            if let crate::ast::Item::Unit(unit) = item {
                registry.define_unit(&unit.name, &unit.value).unwrap();
            }
        }
        let millimeter = &registry.units["Millimeter"];
        assert_eq!(millimeter.dimension, registry.units["Meter"].dimension);
        assert_eq!(millimeter.scale, BigRational::new(1.into(), 1000.into()));
        assert_eq!(registry.units["Density"].dimension.to_string(), "L^-3·M");
    }

    #[test]
    fn only_the_already_reported_type_is_permissive() {
        let length = Type::Quantity {
            dimension: Dimension::base("L"),
            display: "Meter".into(),
            scale: Some(BigRational::one()),
            explicit_presentation: false,
            absolute: false,
        };

        // `Error` suppresses cascading diagnostics.
        assert!(compatible(&Type::Error, &length));
        assert!(compatible(&length, &Type::Error));

        // `Unknown` means nothing was inferred and nothing was reported, so it
        // must not launder a mismatch.
        assert!(!compatible(&Type::Unknown, &length));
        assert!(!compatible(&length, &Type::Unknown));

        // Text is never a quantity, a number, or a bool.
        assert!(!compatible(&Type::String, &length));
        assert!(!compatible(&Type::String, &Type::Integer));
        assert!(!compatible(&Type::String, &Type::Scalar));
        assert!(!compatible(&Type::String, &Type::Bool));
        assert!(compatible(&Type::String, &Type::String));
    }

    /// One representative from every registered family, so a typo in a scale
    /// is caught here rather than in a model six months from now.
    #[test]
    fn builtin_unit_scales_are_exact() {
        let registry = TypeRegistry::default();
        let scale = |name: &str| registry.units[name].scale.clone();
        let exact = |numerator: i128, denominator: i128| {
            BigRational::new(numerator.into(), denominator.into())
        };

        // SI derived: all canonical, scale one.
        for name in [
            "Hertz",
            "Newton",
            "Pascal",
            "Joule",
            "Watt",
            "Coulomb",
            "Volt",
            "Ohm",
            "Siemens",
            "Farad",
            "Weber",
            "Tesla",
            "Henry",
            "Lumen",
            "Lux",
            "Becquerel",
            "Gray",
            "Sievert",
            "Katal",
        ] {
            assert!(scale(name).is_one(), "{name} should be canonical");
        }
        assert_eq!(registry.units["Watt"].dimension.to_string(), "L^2·M·T^-3");
        assert_eq!(
            registry.units["Volt"].dimension.to_string(),
            "I^-1·L^2·M·T^-3"
        );

        // Kinematic and mechanical dimension names.
        assert_eq!(registry.units["Velocity"].dimension.to_string(), "L·T^-1");
        assert_eq!(
            registry.units["Speed"].dimension,
            registry.units["Velocity"].dimension
        );
        assert_eq!(registry.units["Density"].dimension.to_string(), "L^-3·M");
        assert_eq!(
            registry.units["VolumetricFlowRate"].dimension.to_string(),
            "L^3·T^-1"
        );

        // Calendar time. `Month` is deliberately not registered.
        assert_eq!(scale("Minute"), exact(60, 1));
        assert_eq!(scale("Hour"), exact(3_600, 1));
        assert_eq!(scale("Day"), exact(86_400, 1));
        assert_eq!(scale("Week"), exact(604_800, 1));
        assert_eq!(scale("Year"), exact(31_557_600, 1));
        assert!(!registry.units.contains_key("Month"));

        // Information, decimal and binary.
        assert_eq!(scale("Bit"), exact(1, 8));
        assert_eq!(scale("Byte"), BigRational::one());
        assert_eq!(scale("Gigabit"), exact(125_000_000, 1));
        assert_eq!(scale("Gigabyte"), scale("Gigabit") * exact(8, 1));
        assert_eq!(scale("Kibibyte"), exact(1_024, 1));
        assert_eq!(scale("Tebibyte"), exact(1_099_511_627_776, 1));
        assert_eq!(scale("GigabitPerSecond"), scale("Gigabit"));
        assert_eq!(
            registry.units["GigabitPerSecond"].dimension,
            registry.units["Information"]
                .dimension
                .divide(&registry.units["Second"].dimension)
        );

        // US customary, from the 1959 international definitions.
        assert_eq!(scale("Inch"), exact(127, 5_000));
        assert_eq!(scale("Foot"), exact(381, 1_250));
        assert_eq!(scale("Mile"), exact(201_168, 125));
        assert_eq!(scale("Pound"), exact(45_359_237, 100_000_000));
        assert_eq!(scale("Ounce"), scale("Pound") / exact(16, 1));
        assert_eq!(scale("Gallon"), exact(473_176_473, 125_000_000_000));
        assert_eq!(scale("Liter"), exact(1, 1_000));
        assert_eq!(scale("PSI"), exact(8_896_443_230_521, 1_290_320_000));
        assert_eq!(scale("BTU"), exact(105_505_585_262, 100_000_000));
        assert_eq!(
            scale("Horsepower"),
            exact(37_284_993_579_113_511, 50_000_000_000_000)
        );
        assert_eq!(scale("Acre"), exact(316_160_658, 78_125));

        // Angle and dimensionless. `Degree` is deliberately absent: pi/180 has
        // no exact rational representation.
        assert!(scale("Radian").is_one());
        assert!(scale("Steradian").is_one());
        assert!(scale("Count").is_one());
        assert_eq!(scale("Percent"), exact(1, 100));
        assert_eq!(scale("PPM"), exact(1, 1_000_000));
        assert!(!registry.units.contains_key("Degree"));

        // Curated SI prefixes.
        assert_eq!(scale("Millimeter"), exact(1, 1_000));
        assert_eq!(scale("Nanometer"), exact(1, 1_000_000_000));
        assert_eq!(scale("Tonne"), exact(1_000, 1));
        assert_eq!(scale("Megapascal"), exact(1_000_000, 1));
        assert_eq!(scale("KilowattHour"), exact(3_600_000, 1));

        // Currency stays its own base dimension and gains no companions.
        assert_eq!(registry.units["USD"].dimension, Dimension::base("USD"));
        assert!(!registry.units.contains_key("EUR"));
    }

    #[test]
    fn absolute_temperature_units_convert_exactly_in_both_directions() {
        let registry = TypeRegistry::default();
        let celsius = &registry.units["Celsius"];
        let fahrenheit = &registry.units["Fahrenheit"];
        let kelvin = &registry.units["Kelvin"];
        let rankine = &registry.units["Rankine"];
        let exact = |numerator: i128, denominator: i128| {
            BigRational::new(numerator.into(), denominator.into())
        };

        assert!(celsius.is_absolute());
        assert!(fahrenheit.is_absolute());
        assert!(!kelvin.is_absolute());
        assert!(!rankine.is_absolute());

        // The reference points, exactly.
        assert_eq!(
            fahrenheit.to_canonical(&exact(32, 1)),
            exact(5_463, 20),
            "32 F is 273.15 K"
        );
        assert_eq!(
            fahrenheit.to_canonical(&exact(212, 1)),
            exact(7_463, 20),
            "212 F is 373.15 K"
        );
        assert_eq!(celsius.to_canonical(&BigRational::zero()), exact(5_463, 20));

        // Round trips.
        for value in [-40_i128, 0, 32, 98, 212] {
            let point = exact(value, 1);
            assert_eq!(
                fahrenheit.from_canonical(&fahrenheit.to_canonical(&point)),
                point
            );
            assert_eq!(celsius.from_canonical(&celsius.to_canonical(&point)), point);
        }

        // 42 C is exactly 107.6 F, not a rounded float.
        assert_eq!(
            fahrenheit.from_canonical(&celsius.to_canonical(&exact(42, 1))),
            exact(538, 5)
        );
        // -40 is the crossing point of the two scales.
        assert_eq!(
            fahrenheit.from_canonical(&celsius.to_canonical(&exact(-40, 1))),
            exact(-40, 1)
        );

        // Interval aliases carry no offset and pair with the point scales.
        assert_eq!(
            registry
                .relative_counterpart(celsius)
                .map(|unit| &unit.name),
            Some(&"Kelvin".to_owned())
        );
        assert_eq!(
            registry
                .relative_counterpart(fahrenheit)
                .map(|unit| &unit.name),
            Some(&"Rankine".to_owned())
        );
        assert_eq!(registry.units["CelsiusDegree"].scale, kelvin.scale);
        assert!(!registry.units["CelsiusDegree"].is_absolute());
        assert_eq!(registry.units["FahrenheitDegree"].scale, rankine.scale);
        assert!(!registry.units["FahrenheitDegree"].is_absolute());
    }

    #[test]
    fn absolute_units_cannot_be_composed_into_other_units() {
        let parsed = parse("unit PerSecond = Celsius / Second");
        assert!(parsed.errors.is_empty(), "{:#?}", parsed.errors);
        let mut registry = TypeRegistry::default();
        let crate::ast::Item::Unit(unit) = &parsed.module.items[0] else {
            panic!("expected a unit declaration");
        };
        let error = registry.define_unit(&unit.name, &unit.value).unwrap_err();
        assert!(error.contains("Celsius"), "{error}");
        assert!(error.contains("Kelvin"), "{error}");
    }

    /// Concrete units are a presentation choice; dimension names are not. This
    /// classification must come from the registry, not a hardcoded list, now
    /// that around a hundred names are built in.
    #[test]
    fn base_dimensions_are_distinguished_from_concrete_units() {
        let parsed = parse("unit Information = base unit Byte = Information unit Hertz = Second");
        assert!(parsed.errors.is_empty(), "{:#?}", parsed.errors);
        let mut registry = TypeRegistry::default();
        for item in parsed.module.items {
            if let crate::ast::Item::Unit(unit) = item {
                registry.define_unit(&unit.name, &unit.value).unwrap();
            }
        }
        let base_dimension = |name: &str| registry.units[name].is_base_dimension;

        // Built-in dimension names.
        for name in [
            "Scalar", "Length", "Mass", "Time", "Area", "Volume", "Energy",
        ] {
            assert!(base_dimension(name), "{name} names a dimension");
        }
        // Built-in concrete units.
        for name in ["Meter", "Kilogram", "Second", "Joule", "Kelvin", "Celsius"] {
            assert!(!base_dimension(name), "{name} names a concrete unit");
        }
        // A user's own base dimension classifies like a built-in one, and a
        // unit derived from it does not.
        assert!(base_dimension("Information"));
        assert!(!base_dimension("Byte"));
        // Shadowing a built-in dimension name with a concrete definition
        // reclassifies it, because the user's definition wins outright.
        assert!(!base_dimension("Hertz"));
        assert_eq!(registry.units["Hertz"].dimension, Dimension::base("T"));
    }

    /// The hand-written definitions this test used to rely on are now built in.
    /// Re-declaring them must be silent and must win, and must land on exactly
    /// the same scales.
    #[test]
    fn converts_decimal_bits_bytes_and_rates_exactly() {
        let parsed = parse(
            r#"
                unit Information = base
                unit Byte = Information
                unit Bit = Byte / 8
                unit Gigabyte = Byte * 1000000000
                unit Gigabit = Bit * 1000000000
                unit Petabyte = Byte * 1000000000000000
                unit Exabyte = Byte * 1000000000000000000
                unit GigabitPerSecond = Gigabit / Second
            "#,
        );
        assert!(parsed.errors.is_empty(), "{:#?}", parsed.errors);
        let mut registry = TypeRegistry::default();
        for item in parsed.module.items {
            if let crate::ast::Item::Unit(unit) = item {
                registry.define_unit(&unit.name, &unit.value).unwrap();
            }
        }

        assert_eq!(
            registry.units["Bit"].scale,
            BigRational::new(1.into(), 8.into())
        );
        assert_eq!(registry.units["Byte"].dimension.to_string(), "Information");
        assert_ne!(
            registry.units["Byte"].dimension,
            registry.units["Scalar"].dimension
        );
        assert_eq!(
            registry.units["Gigabit"].scale,
            BigRational::from_integer(125_000_000.into())
        );
        assert_eq!(
            registry.units["Gigabyte"].scale,
            registry.units["Gigabit"].scale.clone() * BigRational::from_integer(8.into())
        );
        assert_eq!(
            registry.units["Petabyte"].scale,
            BigRational::from_integer(1_000_000_000_000_000_u64.into())
        );
        assert_eq!(
            registry.units["Exabyte"].scale,
            BigRational::from_integer(1_000_000_000_000_000_000_u64.into())
        );
        assert_eq!(
            registry.units["GigabitPerSecond"].dimension,
            registry.units["Information"]
                .dimension
                .divide(&registry.units["Second"].dimension)
        );
    }
}
