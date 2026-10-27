//! Bind module-qualified names before the legacy name-keyed semantic passes.
//!
//! Each declaration has one key, independent of the aliases used to reach it.
//! The original CST, source text, and spans remain unchanged. Only semantic AST
//! names are lowered; lexical locals and member names are never renamed.
use std::collections::{HashMap, HashSet};

use crate::ast::*;
use crate::source::SourceSet;

pub(crate) type BindingError = (String, Span, String);

pub(crate) fn target_key(sources: &SourceSet, file: &str, path: &ImportPath) -> Option<String> {
    match path {
        ImportPath::Relative(relative) => {
            crate::source::source_path(&sources.files[file].path.as_ref()?.parent()?.join(relative))
                .ok()
                .map(|p| p.to_string_lossy().into_owned())
        }
        ImportPath::Module(name) if name.starts_with("std.") => Some(format!("std:{name}")),
        _ => None,
    }
}

pub(crate) fn bind(sources: &mut SourceSet) -> Vec<BindingError> {
    let mut errors = Vec::new();
    let mut qualified_files = HashSet::new();
    let mut counts = HashMap::<String, usize>::new();
    let mut has_alias = false;
    for (key, file) in &sources.files {
        for item in &file.parse.module.items {
            if let Some(name) = item.name() {
                *counts.entry(name.into()).or_default() += 1;
            }
            if let Item::Import(import) = item {
                if import.alias.is_some() {
                    has_alias = true;
                    if let Some(target) = target_key(sources, key, &import.path) {
                        qualified_files.insert(target);
                    }
                }
                if matches!(import.path, ImportPath::Relative(_))
                    && import.alias.is_none()
                    && import.names.is_empty()
                {
                    errors.push((key.clone(), import.span.clone(),
                        "quoted import needs a namespace (`as module`) or an explicit member list (`(plan)`); bare imports do not import any names".into()));
                }
            }
        }
    }
    // Preserve legacy declaration keys for programs without namespace imports.
    let declarations = sources
        .files
        .iter()
        .map(|(key, file)| {
            let names = file
                .parse
                .module
                .items
                .iter()
                .filter_map(Item::name)
                .map(|name| {
                    let canonical =
                        if has_alias && (qualified_files.contains(key) || counts[name] > 1) {
                            // A valid identifier survives exact-expression reparsing in
                            // constraint propagation; the report retains the source path.
                            format!(
                                "{name}__module_{}",
                                crate::diagnostic::stable_fingerprint(&file.display_path)
                            )
                        } else {
                            name.into()
                        };
                    (name.to_owned(), canonical)
                })
                .collect::<HashMap<_, _>>();
            (key.clone(), names)
        })
        .collect::<HashMap<_, _>>();

    let mut scopes = HashMap::new();
    for (key, file) in &sources.files {
        let mut scope = Scope {
            names: declarations[key].clone(),
            namespaces: HashMap::new(),
            native: HashMap::new(),
            errors: Vec::new(),
        };
        let mut occupied = scope.names.keys().cloned().collect::<HashSet<_>>();
        for item in &file.parse.module.items {
            let Item::Import(import) = item else { continue };
            let target = target_key(sources, key, &import.path);
            if let Some(alias) = &import.alias {
                if matches!(alias.as_str(), "std" | "math") || !occupied.insert(alias.clone()) {
                    scope.errors.push((import.span.clone(), format!("namespace {alias} conflicts with an existing binding or reserved native namespace")));
                }
                match &import.path {
                    ImportPath::Module(module) if module == "math" || module == "std.stream" => {
                        scope.native.insert(alias.clone(), module.clone());
                    }
                    _ => {
                        if let Some(exports) = target.as_ref().and_then(|t| declarations.get(t)) {
                            scope.namespaces.insert(alias.clone(), exports.clone());
                        }
                    }
                }
            } else if let Some(exports) = target.as_ref().and_then(|t| declarations.get(t)) {
                for name in &import.names {
                    if !occupied.insert(name.clone()) {
                        scope.errors.push((
                            import.span.clone(),
                            format!("imported name {name} conflicts with an existing binding"),
                        ));
                    }
                    if let Some(canonical) = exports.get(name) {
                        scope.names.insert(name.clone(), canonical.clone());
                    }
                }
            }
        }
        scopes.insert(key.clone(), scope);
    }

    // Capture import destinations before mutably visiting the source set.
    let targets = sources
        .files
        .iter()
        .map(|(key, file)| {
            (
                key.clone(),
                file.parse
                    .module
                    .items
                    .iter()
                    .map(|item| {
                        if let Item::Import(import) = item {
                            target_key(sources, key, &import.path)
                        } else {
                            None
                        }
                    })
                    .collect::<Vec<_>>(),
            )
        })
        .collect::<HashMap<_, _>>();
    for (key, file) in &mut sources.files {
        let scope = scopes.get_mut(key).unwrap();
        for (index, item) in file.parse.module.items.iter_mut().enumerate() {
            if let Item::Import(import) = item {
                if let Some(exports) = targets[key][index]
                    .as_ref()
                    .and_then(|t| declarations.get(t))
                {
                    if import.alias.is_some() {
                        // Existing visibility checking consumes these resolved keys.
                        import.names = exports.values().cloned().collect();
                        import.names.sort();
                    } else {
                        for name in &mut import.names {
                            if let Some(canonical) = exports.get(name) {
                                *name = canonical.clone();
                            }
                        }
                    }
                }
            } else {
                scope.item(item);
            }
        }
        errors.extend(
            scope
                .errors
                .drain(..)
                .map(|(span, message)| (key.clone(), span, message)),
        );
    }
    if has_alias {
        // Imported unit aliases and refinements must be defined before users.
        fn visit(
            key: &str,
            targets: &HashMap<String, Vec<Option<String>>>,
            seen: &mut HashSet<String>,
            order: &mut Vec<String>,
        ) {
            if !seen.insert(key.into()) {
                return;
            }
            if let Some(children) = targets.get(key) {
                for child in children.iter().flatten() {
                    visit(child, targets, seen, order);
                }
            }
            order.push(key.into());
        }
        let mut order = Vec::new();
        let mut seen = HashSet::new();
        for key in sources.files.keys() {
            visit(key, &targets, &mut seen, &mut order);
        }
        let mut original = std::mem::take(&mut sources.files);
        for key in order {
            if let Some(file) = original.shift_remove(&key) {
                sources.files.insert(key, file);
            }
        }
    }
    errors
}

struct Scope {
    names: HashMap<String, String>,
    namespaces: HashMap<String, HashMap<String, String>>,
    native: HashMap<String, String>,
    errors: Vec<(Span, String)>,
}

impl Scope {
    fn name(&mut self, name: &mut String, span: &Span, bound: &HashSet<String>) {
        if bound.contains(name) {
            return;
        }
        if let Some((alias, member)) = name.split_once('.') {
            if bound.contains(alias) {
                return;
            }
            if let Some(exports) = self.namespaces.get(alias) {
                if let Some(canonical) = exports.get(member) {
                    *name = canonical.clone();
                } else {
                    self.errors.push((
                        span.clone(),
                        format!("namespace {alias} does not export {member}"),
                    ));
                }
                return;
            }
        }
        if let Some(canonical) = self.names.get(name) {
            *name = canonical.clone();
        }
    }

    fn ty(&mut self, ty: &mut TypeExpr, bound: &HashSet<String>) {
        if let Some(fields) = &mut ty.fields {
            for field in fields {
                self.ty(&mut field.ty, bound);
            }
        }
        if let Some(dimension) = &mut ty.dimension {
            self.expr(dimension, bound);
            ty.name = dimension.display();
        } else {
            self.name(&mut ty.name, &ty.span, bound);
        }
        for arg in &mut ty.arguments {
            if let TypeArgument::Type(ty) = arg {
                self.ty(ty, bound);
            }
        }
        for array in &mut ty.arrays {
            if let ArraySize::Fixed(text) = array {
                if let Ok(mut expression) = crate::parser::parse_expression(text) {
                    self.expr(&mut expression, bound);
                    *text = expression.display();
                }
            }
        }
    }

    fn expr(&mut self, expr: &mut Expr, bound: &HashSet<String>) {
        if let ExprKind::Member { value, member } = &expr.kind {
            if let Some(alias) = value.as_name() {
                if !bound.contains(alias) {
                    if let Some(module) = self.native.get(alias) {
                        let path = format!("{module}.{member}");
                        // Native calls inspect the dotted path; math constants
                        // additionally inspect the member receiver.
                        let mut parts = path.split('.');
                        let mut result = Expr::name(parts.next().unwrap(), expr.span.clone());
                        for part in parts {
                            result = Expr {
                                kind: ExprKind::Member {
                                    value: Box::new(result),
                                    member: part.into(),
                                },
                                span: expr.span.clone(),
                            };
                        }
                        *expr = result;
                        return;
                    }
                    if self.namespaces.contains_key(alias) {
                        let mut name = format!("{alias}.{member}");
                        self.name(&mut name, &expr.span, bound);
                        expr.kind = ExprKind::Name { value: name };
                        return;
                    }
                }
            }
        }
        match &mut expr.kind {
            ExprKind::Name { value } => self.name(value, &expr.span, bound),
            ExprKind::Number {
                suffix: Some(suffix),
                ..
            } => self.name(suffix, &expr.span, &HashSet::new()),
            ExprKind::Unary { value, .. } | ExprKind::Member { value, .. } => {
                self.expr(value, bound)
            }
            ExprKind::Presentation { value, unit, .. } => {
                self.expr(value, bound);
                if let Some(unit) = unit {
                    self.expr(unit, &HashSet::new());
                }
            }
            ExprKind::Binary { left, right, .. } => {
                self.expr(left, bound);
                self.expr(right, bound);
            }
            ExprKind::Index { value, index, .. } => {
                self.expr(value, bound);
                if let Some(index) = index {
                    self.expr(index, bound);
                }
            }
            ExprKind::Array { items } => {
                for value in items {
                    self.expr(value, bound);
                }
            }
            ExprKind::NamedTuple { fields } => {
                for field in fields {
                    self.expr(&mut field.value, bound);
                }
            }
            ExprKind::Call { callee, arguments } => {
                self.expr(callee, bound);
                for argument in arguments {
                    self.expr(&mut argument.value, bound);
                }
            }
            ExprKind::Lambda { parameters, body } => {
                let mut bound = bound.clone();
                bound.extend(parameters.iter().cloned());
                self.expr(body, &bound);
            }
            _ => {}
        }
    }

    fn block(&mut self, block: &mut Block, bound: &HashSet<String>) {
        let mut bound = bound.clone();
        for stmt in &mut block.statements {
            match stmt {
                Stmt::Let {
                    name, ty, value, ..
                } => {
                    if let Some(ty) = ty {
                        self.ty(ty, &bound);
                    }
                    self.expr(value, &bound);
                    bound.insert(name.clone());
                }
                Stmt::Requires { value, .. } | Stmt::Expr { value, .. } => self.expr(value, &bound),
                Stmt::Assign { target, value, .. }
                | Stmt::Wire {
                    source: value,
                    target,
                    ..
                } => {
                    self.expr(target, &bound);
                    self.expr(value, &bound);
                }
                Stmt::Before { first, second, .. } => {
                    self.expr(first, &bound);
                    self.expr(second, &bound);
                }
                Stmt::Yield {
                    target,
                    value,
                    after,
                    ..
                } => {
                    self.expr(target, &bound);
                    self.expr(value, &bound);
                    if let Some(after) = after {
                        self.expr(after, &bound);
                    }
                }
                Stmt::For {
                    binding,
                    iterable,
                    body,
                    ..
                } => {
                    self.expr(iterable, &bound);
                    let mut inner = bound.clone();
                    inner.insert(binding.clone());
                    self.block(body, &inner);
                }
                Stmt::Repeat { count, body, .. } => {
                    self.expr(count, &bound);
                    self.block(body, &bound);
                }
                Stmt::If {
                    condition,
                    then_branch,
                    else_branch,
                    ..
                } => {
                    self.expr(condition, &bound);
                    self.block(then_branch, &bound);
                    if let Some(other) = else_branch {
                        self.block(other, &bound);
                    }
                }
            }
        }
    }

    fn function(&mut self, function: &mut Function, bound: &HashSet<String>) {
        let mut bound = bound.clone();
        bound.extend(function.generics.iter().cloned());
        for parameter in function.parameters.iter_mut().chain(&mut function.outputs) {
            self.ty(&mut parameter.ty, &bound);
        }
        for constraint in &mut function.bounds {
            self.ty(&mut constraint.capability, &bound);
        }
        bound.extend(
            function
                .parameters
                .iter()
                .chain(&function.outputs)
                .map(|p| p.name.clone()),
        );
        if let Some(body) = &mut function.body {
            self.block(body, &bound);
        }
    }

    fn item(&mut self, item: &mut Item) {
        let empty = HashSet::new();
        match item {
            Item::Unit(unit) => {
                self.name(&mut unit.name, &unit.span, &empty);
                let mut bound = empty.clone();
                if unit.refinement.is_some() {
                    if let ExprKind::Call { arguments, .. } = &unit.value.kind {
                        for arg in arguments {
                            if let Some(name) = arg.value.as_name() {
                                bound.insert(name.into());
                            }
                        }
                    }
                }
                self.expr(&mut unit.value, &bound);
                if let Some(body) = &mut unit.refinement {
                    self.block(body, &bound);
                }
            }
            Item::Artifact(artifact) => {
                self.name(&mut artifact.name, &artifact.span, &empty);
                let bound = artifact.fields.iter().map(|f| f.name.clone()).collect();
                for field in &mut artifact.fields {
                    self.ty(&mut field.ty, &empty);
                    if let Some(value) = &mut field.derived {
                        self.expr(value, &bound);
                    }
                }
            }
            Item::Resource(resource) => {
                self.name(&mut resource.name, &resource.span, &empty);
                self.expr(&mut resource.capacity, &empty);
            }
            Item::Process(process) => {
                self.name(&mut process.name, &process.span, &empty);
                for parent in &mut process.parents {
                    self.name(parent, &process.span, &empty);
                }
                let bound = process
                    .members
                    .iter()
                    .map(|m| m.name.clone())
                    .chain(process.parameters.iter().map(|p| p.name.clone()))
                    .collect();
                for member in &mut process.members {
                    self.ty(&mut member.ty, &empty);
                    if let Some(default) = &mut member.default {
                        self.expr(default, &bound);
                    }
                }
                for parameter in &mut process.parameters {
                    self.ty(&mut parameter.ty, &empty);
                }
                for monitor in &mut process.monitors {
                    if let Some(unit) = &mut monitor.display_unit_expression {
                        let before = unit.display();
                        self.expr(unit, &empty);
                        if before != unit.display() {
                            monitor.display_unit = Some(unit.display());
                        }
                    }
                }
                if let Some(body) = &mut process.body {
                    self.block(body, &bound);
                }
            }
            Item::Function(function) => {
                self.name(&mut function.name, &function.span, &empty);
                self.function(function, &empty);
            }
            Item::Trait(trait_) => {
                self.name(&mut trait_.name, &trait_.span, &empty);
                let bound = trait_.generics.iter().cloned().collect();
                for function in &mut trait_.functions {
                    self.function(function, &bound);
                }
            }
            Item::Impl(impl_) => {
                self.name(&mut impl_.trait_name, &impl_.span, &empty);
                for arg in &mut impl_.arguments {
                    self.ty(arg, &empty);
                }
                for function in &mut impl_.functions {
                    self.function(function, &empty);
                }
            }
            Item::Import(_) => {}
        }
    }
}
