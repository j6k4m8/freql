//! Source-bound navigation queries. Names are bound through AST scopes; strings,
//! comments and unrelated identically spelled fields are never rename targets.
use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::ast::*;
use crate::ide::AnalysisSnapshot;
use crate::source::SourceFile;
use crate::types::Type;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SourceLocation {
    pub path: PathBuf,
    pub range: Span,
}

#[derive(Clone, Debug)]
pub struct HoverInfo {
    pub range: Span,
    pub markdown: String,
}

#[derive(Clone, Debug)]
pub struct ConstraintExplanation {
    pub title: String,
    pub markdown: String,
}

#[derive(Clone)]
struct Symbol {
    name: String,
    location: SourceLocation,
    detail: String,
    doc: Option<String>,
    ty: Option<Type>,
    owner: Option<String>,
    rename: bool,
    scope: Span,
}

#[derive(Clone)]
struct Occurrence {
    symbol: usize,
    location: SourceLocation,
    declaration: bool,
    // Punning couples the public label to a local binding. A local-only rename
    // must not alter that label without expanding the shorthand first.
    punned: bool,
}

struct Navigation<'a> {
    snapshot: &'a AnalysisSnapshot,
    symbols: Vec<Symbol>,
    occurrences: Vec<Occurrence>,
    globals: HashMap<(String, String), usize>,
    members: HashMap<(String, String), usize>,
    canonical: HashMap<(String, usize), String>,
}

impl AnalysisSnapshot {
    pub fn definition_at(&self, path: &Path, offset: usize) -> Option<SourceLocation> {
        let index = Navigation::new(self);
        Some(
            index.symbols[index.at(path, offset)?.symbol]
                .location
                .clone(),
        )
    }

    pub fn references_at(
        &self,
        path: &Path,
        offset: usize,
        declarations: bool,
    ) -> Vec<SourceLocation> {
        let index = Navigation::new(self);
        let Some(at) = index.at(path, offset) else {
            return Vec::new();
        };
        let mut locations = index
            .occurrences
            .iter()
            .filter(|item| item.symbol == at.symbol && (declarations || !item.declaration))
            .map(|item| item.location.clone())
            .collect::<Vec<_>>();
        locations.sort_by_key(|item| (item.path.clone(), item.range.start));
        locations.dedup();
        locations
    }

    pub fn prepare_rename_at(&self, path: &Path, offset: usize) -> Result<SourceLocation, String> {
        let index = Navigation::new(self);
        let at = index
            .at(path, offset)
            .ok_or("No unambiguous symbol at this position")?;
        index.validate_rename(at.symbol)?;
        Ok(at.location.clone())
    }

    pub fn rename_at(
        &self,
        path: &Path,
        offset: usize,
        new_name: &str,
    ) -> Result<Vec<SourceLocation>, String> {
        let index = Navigation::new(self);
        let at = index
            .at(path, offset)
            .ok_or("No unambiguous symbol at this position")?;
        index.validate_rename(at.symbol)?;
        if !valid_identifier(new_name) {
            return Err("The new name must be a non-keyword freql identifier".into());
        }
        let symbol = &index.symbols[at.symbol];
        // Deliberately conservative: a colliding identifier anywhere in this
        // scope may execute an existing reference, including unresolved names.
        let source = index
            .source(&symbol.location.path)
            .ok_or("Source unavailable")?;
        if new_name != symbol.name
            && (index.symbols.iter().any(|other| {
                other.name == new_name
                    && other.owner == symbol.owner
                    && other.location.path == symbol.location.path
            }) || crate::lexer::lex(&source.text).tokens.iter().any(|token| {
                token.text == new_name
                    && token.kind == crate::syntax::SyntaxKind::Ident
                    && token.span.start >= symbol.scope.start
                    && token.span.end <= symbol.scope.end
            }))
        {
            return Err(format!(
                "Renaming to {new_name} could execute or conflict with a name in this scope"
            ));
        }
        let mut locations = index
            .occurrences
            .iter()
            .filter(|item| item.symbol == at.symbol)
            .map(|item| item.location.clone())
            .collect::<Vec<_>>();
        locations.sort_by_key(|item| (item.path.clone(), item.range.start));
        locations.dedup();
        Ok(locations)
    }

    pub fn hover_at(&self, path: &Path, offset: usize) -> Option<HoverInfo> {
        let index = Navigation::new(self);
        let at = index.at(path, offset)?;
        let symbol = &index.symbols[at.symbol];
        let mut markdown = format!(
            "```req\n{}\n```",
            self.compilation.display_text(&symbol.detail)
        );
        if let Some(doc) = &symbol.doc {
            markdown.push_str(&format!("\n\n{doc}"));
        }
        if let Some(owner) = &symbol.owner {
            if let Some(unit) = self.compilation.model.units.get(owner) {
                for refinement in &unit.refinements {
                    markdown.push_str(&format!("\n\nRefinement: `{refinement}`"));
                }
            }
        }
        let source = index.source(&symbol.location.path)?;
        for item in &source.parse.module.items {
            let declaration_type = match item {
                Item::Process(p) => p
                    .members
                    .iter()
                    .find(|m| {
                        m.name == symbol.name && m.span.contains(&symbol.location.range.start)
                    })
                    .map(|m| &m.ty),
                Item::Artifact(a) => a
                    .fields
                    .iter()
                    .find(|f| {
                        f.name == symbol.name && f.span.contains(&symbol.location.range.start)
                    })
                    .map(|f| &f.ty),
                _ => None,
            };
            if let Some(ty) = declaration_type {
                if let Some(id) = index.lookup(source, &HashMap::new(), &ty.name) {
                    if let Some(unit) = index.symbols[id]
                        .owner
                        .as_ref()
                        .and_then(|key| self.compilation.model.units.get(key))
                    {
                        for refinement in &unit.refinements {
                            markdown.push_str(&format!("\n\nRefinement: `{refinement}`"));
                        }
                    }
                }
            }
        }
        // Only compiler-emitted values are shown, with exact instance paths.
        // Multiple instantiations stay separate rather than claiming a single
        // value for a shared declaration.
        let unique_binding = index
            .symbols
            .iter()
            .filter(|s| {
                s.name == symbol.name
                    && s.owner == symbol.owner
                    && s.location.path == symbol.location.path
            })
            .count()
            == 1;
        for metric in self
            .compilation
            .report
            .monitored_metrics
            .iter()
            .filter(|metric| {
                unique_binding
                    && metric.span.path == source.display_path
                    && metric.path.rsplit('.').next() == Some(&symbol.name)
                    && symbol.scope.start <= metric.span.start
                    && metric.span.start <= symbol.scope.end
            })
            .take(8)
        {
            markdown.push_str(&format!(
                "\n\n**`{}`** (entry `{}`): `{}`",
                self.compilation.display_text(&metric.path),
                self.compilation.report.entry,
                self.compilation.display_text(metric.display_value())
            ));
            if !metric.dependencies.is_empty() {
                markdown.push_str("\n\nUnresolved dependencies:");
                for dependency in &metric.dependencies {
                    markdown.push_str(&format!("\n- `{dependency}`"));
                }
            }
        }
        if self.entry_context.configured_entry.is_some()
            && !self.entry_context.uses_configured_entry
        {
            markdown.push_str("\n\nThis file is outside the selected entry's import closure; analyzed independently.");
        }
        Some(HoverInfo {
            range: at.location.range.clone(),
            markdown,
        })
    }

    /// Explain actual emitted compiler evidence, never a reconstructed proof.
    pub fn explain_at(&self, path: &Path, offset: usize) -> Option<ConstraintExplanation> {
        let canonical = crate::source::source_path(path).ok()?;
        let source = self
            .sources
            .files
            .values()
            .find(|s| s.path.as_ref() == Some(&canonical))?;
        let mut sections = Vec::new();
        let matches = |span: &crate::diagnostic::SourceSpan| {
            span.path == source.display_path && span.start <= offset && offset <= span.end
        };
        let report = &self.compilation.report;
        for diagnostic in report.diagnostics.iter().filter(|d| matches(&d.span)) {
            let mut text = format!(
                "### {}\n\n{}\n\nOrigin: {}:{}:{}",
                diagnostic.code,
                diagnostic.message,
                diagnostic.span.path,
                diagnostic.span.line,
                diagnostic.span.column
            );
            if let Some(expression) = source.text.get(diagnostic.span.start..diagnostic.span.end) {
                text.push_str(&format!("\n\nSource:\n\n```req\n{expression}\n```"));
            }
            for note in &diagnostic.notes {
                text.push_str(&format!("\n\n{note}"));
            }
            sections.push(text);
        }
        for constraint in report.constraints.iter().filter(|c| matches(&c.span)) {
            sections.push(format!(
                "### {}\n\n`{}`\n\nPhase: {} · status: {}\n\nOrigin: {}:{}:{}{}",
                constraint.path,
                constraint.predicate,
                constraint.phase,
                constraint.status,
                constraint.span.path,
                constraint.span.line,
                constraint.span.column,
                dependency_evidence(self, &constraint.dependencies)
            ));
        }
        for obligation in report.obligations.iter().filter(|o| matches(&o.span)) {
            sections.push(format!(
                "### {}\n\n{}\n\nExpected: `{}`\n\nOrigin: {}:{}:{}{}{}",
                obligation.path,
                obligation.message,
                obligation.expected,
                obligation.span.path,
                obligation.span.line,
                obligation.span.column,
                obligation
                    .condition
                    .as_ref()
                    .map(|c| format!("\n\nOn path: `{c}`"))
                    .unwrap_or_default(),
                dependency_evidence(self, &obligation.dependencies)
            ));
        }
        (!sections.is_empty()).then(|| ConstraintExplanation {
            title: "freql constraint evidence".into(),
            markdown: format!(
                "# Constraint evidence\n\nEntry: `{}`\n\n{}",
                report.entry,
                sections.join("\n\n---\n\n")
            ),
        })
    }
}

fn dependency_evidence(snapshot: &AnalysisSnapshot, dependencies: &[String]) -> String {
    let mut text = String::new();
    for dependency in dependencies {
        text.push_str(&format!("\n\nDependency: `{dependency}`"));
        for symbol in snapshot
            .compilation
            .report
            .symbols
            .iter()
            .filter(|s| &s.path == dependency)
        {
            text.push_str(&format!(
                " — {}:{}:{}",
                symbol.span.path, symbol.span.line, symbol.span.column
            ));
        }
        // Values are attached only on exact semantic-path matches.
        for metric in snapshot
            .compilation
            .report
            .monitored_metrics
            .iter()
            .filter(|m| &m.path == dependency)
        {
            text.push_str(&format!("; compiler value: `{}`", metric.value));
        }
    }
    text
}

fn valid_identifier(name: &str) -> bool {
    let mut chars = name.chars();
    matches!(chars.next(), Some(c) if c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
        && ![
            "artifact",
            "process",
            "resource",
            "unit",
            "fn",
            "trait",
            "impl",
            "import",
            "as",
            "base",
            "config",
            "expose",
            "export",
            "gets",
            "uses",
            "monitor",
            "aggregate",
            "let",
            "var",
            "requires",
            "for",
            "in",
            "repeat",
            "if",
            "else",
            "yield",
            "after",
            "before",
            "where",
            "true",
            "false",
            "and",
            "or",
            "not",
        ]
        .contains(&name)
}

impl<'a> Navigation<'a> {
    fn new(snapshot: &'a AnalysisSnapshot) -> Self {
        let mut bound = snapshot.sources.clone();
        crate::namespaces::bind(&mut bound);
        let canonical = bound
            .files
            .iter()
            .flat_map(|(file, source)| {
                source.parse.module.items.iter().filter_map(|item| {
                    item.name()
                        .map(|name| ((file.clone(), item.span().start), name.to_owned()))
                })
            })
            .collect();
        let mut index = Self {
            snapshot,
            symbols: Vec::new(),
            occurrences: Vec::new(),
            globals: HashMap::new(),
            members: HashMap::new(),
            canonical,
        };
        for source in snapshot.sources.files.values() {
            index.declarations(source);
        }
        for source in snapshot.sources.files.values() {
            index.references(source);
        }
        index
    }

    fn source(&self, path: &Path) -> Option<&SourceFile> {
        self.snapshot
            .sources
            .files
            .values()
            .find(|s| s.path.as_deref() == Some(path))
    }

    fn at(&self, path: &Path, offset: usize) -> Option<&Occurrence> {
        let path = crate::source::source_path(path).ok()?;
        self.occurrences
            .iter()
            .filter(|o| {
                o.location.path == path
                    && o.location.range.start <= offset
                    && offset < o.location.range.end
            })
            .min_by_key(|o| o.location.range.len())
            .or_else(|| {
                self.occurrences
                    .iter()
                    .find(|o| o.location.path == path && o.location.range.end == offset)
            })
    }

    fn validate_rename(&self, id: usize) -> Result<(), String> {
        if !self.symbols[id].rename {
            return Err("Safe rename currently supports local let/var and loop bindings. Public members, invocation/function parameters, imports and inherited declarations require whole-workspace reference coverage and cannot yet be renamed safely".into());
        }
        if self
            .snapshot
            .compilation
            .report
            .diagnostics
            .iter()
            .any(|d| d.severity == crate::diagnostic::Severity::Error)
        {
            return Err(
                "Resolve compiler errors before renaming so binding identities are unambiguous"
                    .into(),
            );
        }
        if self.occurrences.iter().any(|o| o.symbol == id && o.punned) {
            return Err(
                "Expand punned arguments to `label: value` before renaming this local binding"
                    .into(),
            );
        }
        Ok(())
    }

    fn token(source: &SourceFile, range: Span, name: &str, last: bool) -> Option<Span> {
        let mut tokens = crate::lexer::lex(source.text.get(range.clone())?)
            .tokens
            .into_iter()
            .filter(|t| t.kind == crate::syntax::SyntaxKind::Ident && t.text == name);
        let token = if last {
            tokens.next_back()?
        } else {
            tokens.next()?
        };
        Some(range.start + token.span.start..range.start + token.span.end)
    }

    fn add(
        &mut self,
        source: &SourceFile,
        name: &str,
        range: Span,
        detail: String,
        ty: Option<Type>,
        owner: Option<String>,
        rename: bool,
        scope: Span,
        doc: Option<String>,
    ) -> Option<usize> {
        let location = SourceLocation {
            path: source.path.clone()?,
            range: Self::token(source, range, name, false)?,
        };
        let id = self.symbols.len();
        self.symbols.push(Symbol {
            name: name.into(),
            location: location.clone(),
            detail,
            doc,
            ty,
            owner,
            rename,
            scope,
        });
        self.occurrences.push(Occurrence {
            symbol: id,
            location,
            declaration: true,
            punned: false,
        });
        Some(id)
    }

    fn refer(
        &mut self,
        source: &SourceFile,
        id: usize,
        range: Span,
        name: &str,
        last: bool,
        punned: bool,
    ) {
        if let (Some(path), Some(range)) =
            (source.path.clone(), Self::token(source, range, name, last))
        {
            self.occurrences.push(Occurrence {
                symbol: id,
                location: SourceLocation { path, range },
                declaration: false,
                punned,
            });
        }
    }

    fn declarations(&mut self, source: &SourceFile) {
        for item in &source.parse.module.items {
            let Some(name) = item.name() else { continue };
            let owner = self
                .canonical
                .get(&(source.key.clone(), item.span().start))
                .cloned()
                .unwrap_or_else(|| name.into());
            let (kind, doc, ty) = match item {
                Item::Process(p) => (
                    "process",
                    p.doc.clone(),
                    Some(Type::ConfiguredProcess(owner.clone())),
                ),
                Item::Artifact(a) => (
                    "artifact",
                    a.doc.clone(),
                    Some(Type::Artifact(owner.clone())),
                ),
                Item::Resource(r) => ("resource", r.doc.clone(), None),
                Item::Function(_) => ("fn", None, Some(Type::Function)),
                Item::Unit(u) => {
                    let detail = format!("unit {name} = {}", u.value.display());
                    if let Some(id) = self.add(
                        source,
                        name,
                        item.span().clone(),
                        detail,
                        None,
                        Some(owner.clone()),
                        false,
                        item.span().clone(),
                        None,
                    ) {
                        self.globals.insert((source.key.clone(), name.into()), id);
                    }
                    continue;
                }
                Item::Trait(_) => ("trait", None, None),
                _ => continue,
            };
            if let Some(id) = self.add(
                source,
                name,
                item.span().clone(),
                format!("{kind} {name}"),
                ty,
                Some(owner.clone()),
                false,
                item.span().clone(),
                doc,
            ) {
                self.globals.insert((source.key.clone(), name.into()), id);
            }
            match item {
                Item::Process(p) => {
                    for member in &p.members {
                        let ty = self
                            .snapshot
                            .compilation
                            .model
                            .processes
                            .get(&owner)
                            .and_then(|p| p.members.get(&member.name))
                            .map(|m| m.ty.clone());
                        let role = match member.role {
                            MemberRole::Config => "config",
                            MemberRole::Gets => "gets",
                            MemberRole::Uses => "uses",
                            MemberRole::Export => "export",
                        };
                        let default = member
                            .default
                            .as_ref()
                            .map(|value| format!("\n# Declared default: {}", value.display()))
                            .unwrap_or_default();
                        if let Some(id) = self.add(
                            source,
                            &member.name,
                            member.span.clone(),
                            format!("{role} {}: {}{default}", member.name, member.ty),
                            ty,
                            Some(owner.clone()),
                            false,
                            p.span.clone(),
                            None,
                        ) {
                            self.members
                                .insert((owner.clone(), member.name.clone()), id);
                        }
                    }
                    for param in &p.parameters {
                        let ty = self
                            .snapshot
                            .compilation
                            .model
                            .processes
                            .get(&owner)
                            .and_then(|p| p.parameters.get(&param.name))
                            .cloned();
                        if let Some(id) = self.add(
                            source,
                            &param.name,
                            param.span.clone(),
                            format!("{}: {}", param.name, param.ty),
                            ty,
                            Some(owner.clone()),
                            false,
                            p.span.clone(),
                            None,
                        ) {
                            self.members.insert((owner.clone(), param.name.clone()), id);
                        }
                    }
                }
                Item::Artifact(a) => {
                    for field in &a.fields {
                        let ty = self
                            .snapshot
                            .compilation
                            .model
                            .artifacts
                            .get(&owner)
                            .and_then(|a| a.fields.get(&field.name))
                            .cloned();
                        if let Some(id) = self.add(
                            source,
                            &field.name,
                            field.span.clone(),
                            format!("{}: {}", field.name, field.ty),
                            ty,
                            Some(owner.clone()),
                            false,
                            a.span.clone(),
                            None,
                        ) {
                            self.members.insert((owner.clone(), field.name.clone()), id);
                        }
                    }
                }
                _ => {}
            }
        }
    }

    fn lookup(
        &self,
        source: &SourceFile,
        scope: &HashMap<String, usize>,
        name: &str,
    ) -> Option<usize> {
        if let Some(id) = scope.get(name) {
            return Some(*id);
        }
        let mut candidates = Vec::new();
        if let Some(id) = self.globals.get(&(source.key.clone(), name.into())) {
            candidates.push(*id);
        }
        for item in &source.parse.module.items {
            let Item::Import(import) = item else { continue };
            if import.alias.is_none() && import.names.iter().any(|n| n == name) {
                if let Some(target) =
                    crate::namespaces::target_key(&self.snapshot.sources, &source.key, &import.path)
                {
                    if let Some(id) = self.globals.get(&(target, name.into())) {
                        candidates.push(*id);
                    }
                }
            }
        }
        candidates.sort();
        candidates.dedup();
        (candidates.len() == 1).then(|| candidates[0])
    }

    fn member(&self, owner: &str, name: &str) -> Option<usize> {
        if let Some(id) = self.members.get(&(owner.into(), name.into())) {
            return Some(*id);
        }
        let inherited = self
            .snapshot
            .compilation
            .model
            .processes
            .get(owner)?
            .members
            .get(name)?;
        let declaring_key = self
            .snapshot
            .compilation
            .model
            .processes
            .iter()
            .find(|(_, process)| process.path == inherited.declared_by)
            .map(|(key, _)| key)?;
        self.members
            .get(&(declaring_key.clone(), name.into()))
            .copied()
    }

    fn references(&mut self, source: &SourceFile) {
        for item in &source.parse.module.items {
            let owner = self
                .canonical
                .get(&(source.key.clone(), item.span().start))
                .cloned()
                .unwrap_or_default();
            let mut scope = HashMap::new();
            match item {
                Item::Process(p) => {
                    if let Some(model) = self.snapshot.compilation.model.processes.get(&owner) {
                        for name in model.members.keys().chain(model.parameters.keys()) {
                            if let Some(id) = self.member(&owner, name) {
                                scope.insert(name.clone(), id);
                            }
                        }
                    }
                    for parent in &p.parents {
                        if let Some(id) = self.lookup(source, &scope, parent) {
                            self.refer(
                                source,
                                id,
                                p.span.start
                                    ..p.members
                                        .first()
                                        .map(|m| m.span.start)
                                        .unwrap_or(p.span.end),
                                parent,
                                true,
                                false,
                            );
                        }
                    }
                    for member in &p.members {
                        self.ty(source, &scope, &member.ty);
                        if let Some(value) = &member.default {
                            self.expr(source, &scope, value, false);
                        }
                    }
                    for param in &p.parameters {
                        self.ty(source, &scope, &param.ty);
                    }
                    if let Some(body) = &p.body {
                        self.block(source, &mut scope, body, &owner);
                    }
                    for monitor in &p.monitors {
                        let root = monitor.name.split('.').next().unwrap_or(&monitor.name);
                        if let Some(id) = self.lookup(source, &scope, root) {
                            self.refer(source, id, monitor.span.clone(), root, false, false);
                        }
                        if let Some(unit) = &monitor.display_unit_expression {
                            self.expr(source, &scope, unit, false);
                        }
                    }
                }
                Item::Artifact(a) => {
                    for field in &a.fields {
                        if let Some(id) = self.member(&owner, &field.name) {
                            scope.insert(field.name.clone(), id);
                        }
                    }
                    for field in &a.fields {
                        self.ty(source, &scope, &field.ty);
                        if let Some(value) = &field.derived {
                            self.expr(source, &scope, value, false);
                        }
                    }
                }
                Item::Function(f) => {
                    for param in f.parameters.iter().chain(&f.outputs) {
                        self.ty(source, &scope, &param.ty);
                        if let Some(id) = self.add(
                            source,
                            &param.name,
                            param.span.clone(),
                            format!("{}: {}", param.name, param.ty),
                            None,
                            Some(owner.clone()),
                            false,
                            f.span.clone(),
                            None,
                        ) {
                            scope.insert(param.name.clone(), id);
                        }
                    }
                    if let Some(body) = &f.body {
                        self.block(source, &mut scope, body, &owner);
                    }
                }
                Item::Unit(u) => {
                    self.expr(source, &scope, &u.value, false);
                }
                Item::Resource(r) => {
                    self.expr(source, &scope, &r.capacity, false);
                }
                Item::Import(import) => {
                    for name in &import.names {
                        if let Some(id) = self.lookup(source, &scope, name) {
                            self.refer(source, id, import.span.clone(), name, true, false);
                        }
                    }
                }
                _ => {}
            }
        }
    }

    fn ty(&mut self, source: &SourceFile, scope: &HashMap<String, usize>, ty: &TypeExpr) {
        // Fixed-array lengths and const generic arguments currently retain
        // their expression as text without component spans. Disable rename of
        // any lexical local referenced there until those spans are available.
        for constant in ty
            .arrays
            .iter()
            .filter_map(|size| match size {
                ArraySize::Fixed(value) => Some(value),
                _ => None,
            })
            .chain(ty.arguments.iter().filter_map(|arg| match arg {
                TypeArgument::Const(value) => Some(value),
                _ => None,
            }))
        {
            for token in crate::lexer::lex(constant).tokens {
                if let Some(id) = scope.get(&token.text) {
                    self.symbols[*id].rename = false;
                }
            }
        }
        if let Some(expr) = &ty.dimension {
            self.expr(source, scope, expr, false);
        } else if let Some(id) = self.lookup(source, scope, &ty.name) {
            self.refer(source, id, ty.span.clone(), &ty.name, false, false);
        }
        for arg in &ty.arguments {
            if let TypeArgument::Type(ty) = arg {
                self.ty(source, scope, ty);
            }
        }
        if let Some(fields) = &ty.fields {
            for field in fields {
                self.ty(source, scope, &field.ty);
            }
        }
    }

    fn block(
        &mut self,
        source: &SourceFile,
        scope: &mut HashMap<String, usize>,
        block: &Block,
        owner: &str,
    ) {
        for stmt in &block.statements {
            match stmt {
                Stmt::Let {
                    name,
                    ty,
                    value,
                    span,
                    mutable,
                } => {
                    let inferred = self.expr(source, scope, value, false);
                    let inferred = self
                        .snapshot
                        .compilation
                        .model
                        .processes
                        .get(owner)
                        .filter(|p| p.binding_declarations.get(name) == Some(&span.end))
                        .and_then(|p| p.bindings.get(name))
                        .cloned()
                        .or(inferred);
                    if let Some(ty) = ty {
                        self.ty(source, scope, ty);
                    }
                    let detail = format!(
                        "{} {name}: {}",
                        if *mutable { "var" } else { "let" },
                        ty.as_ref()
                            .map(ToString::to_string)
                            .or_else(|| inferred.as_ref().map(ToString::to_string))
                            .unwrap_or_else(|| "unresolved".into())
                    );
                    if let Some(id) = self.add(
                        source,
                        name,
                        span.start..value.span.start,
                        detail,
                        inferred,
                        Some(owner.into()),
                        true,
                        block.span.clone(),
                        None,
                    ) {
                        scope.insert(name.clone(), id);
                    }
                }
                Stmt::For {
                    binding,
                    iterable,
                    body,
                    span,
                } => {
                    let ty = match self.expr(source, scope, iterable, false) {
                        Some(Type::Array { element, .. } | Type::Stream(element)) => Some(*element),
                        _ => None,
                    };
                    let mut nested = scope.clone();
                    if let Some(id) = self.add(
                        source,
                        binding,
                        span.start..iterable.span.start,
                        format!(
                            "{binding}: {}",
                            ty.as_ref()
                                .map(ToString::to_string)
                                .unwrap_or_else(|| "unresolved".into())
                        ),
                        ty,
                        Some(owner.into()),
                        true,
                        body.span.clone(),
                        None,
                    ) {
                        nested.insert(binding.clone(), id);
                    }
                    self.block(source, &mut nested, body, owner);
                }
                Stmt::Repeat { count, body, .. } => {
                    self.expr(source, scope, count, false);
                    self.block(source, &mut scope.clone(), body, owner);
                }
                Stmt::If {
                    condition,
                    then_branch,
                    else_branch,
                    ..
                } => {
                    self.expr(source, scope, condition, false);
                    self.block(source, &mut scope.clone(), then_branch, owner);
                    if let Some(body) = else_branch {
                        self.block(source, &mut scope.clone(), body, owner);
                    }
                }
                Stmt::Requires { value, .. } | Stmt::Expr { value, .. } => {
                    self.expr(source, scope, value, false);
                }
                Stmt::Assign { target, value, .. }
                | Stmt::Wire {
                    source: target,
                    target: value,
                    ..
                }
                | Stmt::Before {
                    first: target,
                    second: value,
                    ..
                } => {
                    self.expr(source, scope, target, false);
                    self.expr(source, scope, value, false);
                }
                Stmt::Yield {
                    target,
                    value,
                    after,
                    ..
                } => {
                    self.expr(source, scope, target, false);
                    self.expr(source, scope, value, false);
                    if let Some(after) = after {
                        self.expr(source, scope, after, false);
                    }
                }
            }
        }
    }

    fn expr(
        &mut self,
        source: &SourceFile,
        scope: &HashMap<String, usize>,
        expr: &Expr,
        punned: bool,
    ) -> Option<Type> {
        match &expr.kind {
            ExprKind::Name { value } => {
                let id = self.lookup(source, scope, value)?;
                self.refer(source, id, expr.span.clone(), value, false, punned);
                self.symbols[id].ty.clone()
            }
            ExprKind::Member { value, member } => {
                if let ExprKind::Name { value: alias } = &value.kind {
                    if !scope.contains_key(alias) {
                        for item in &source.parse.module.items {
                            if let Item::Import(import) = item {
                                if import.alias.as_ref() == Some(alias) {
                                    let target = crate::namespaces::target_key(
                                        &self.snapshot.sources,
                                        &source.key,
                                        &import.path,
                                    )?;
                                    let id = *self.globals.get(&(target, member.clone()))?;
                                    self.refer(
                                        source,
                                        id,
                                        value.span.end..expr.span.end,
                                        member,
                                        true,
                                        false,
                                    );
                                    return self.symbols[id].ty.clone();
                                }
                            }
                        }
                    }
                }
                match self.expr(source, scope, value, false)? {
                    Type::Artifact(owner) | Type::Run(owner) | Type::ConfiguredProcess(owner) => {
                        let id = self.member(&owner, member)?;
                        self.refer(
                            source,
                            id,
                            value.span.end..expr.span.end,
                            member,
                            true,
                            false,
                        );
                        self.symbols[id].ty.clone()
                    }
                    Type::NamedTuple(fields) => fields
                        .into_iter()
                        .find(|(name, _)| name == member)
                        .map(|(_, ty)| ty),
                    _ => None,
                }
            }
            ExprKind::Call { callee, arguments } => {
                let ty = self.expr(source, scope, callee, false);
                let owner = match &ty {
                    Some(Type::ConfiguredProcess(owner) | Type::Artifact(owner)) => {
                        Some(owner.clone())
                    }
                    _ => None,
                };
                for argument in arguments {
                    let pun = argument.name.is_none()
                        && matches!(&argument.value.kind, ExprKind::Name { .. });
                    self.expr(source, scope, &argument.value, pun);
                    if !pun {
                        if let (Some(owner), Some(name)) = (&owner, &argument.name) {
                            if let Some(id) = self.member(owner, name) {
                                self.refer(
                                    source,
                                    id,
                                    argument.span.start..argument.value.span.start,
                                    name,
                                    false,
                                    false,
                                );
                            }
                        }
                    }
                }
                match ty {
                    Some(Type::ConfiguredProcess(owner)) => {
                        let construction = match &callee.kind {
                            ExprKind::Name { value } => !scope.contains_key(value),
                            ExprKind::Member { value, .. } => {
                                value.path().is_some_and(|name| !scope.contains_key(&name))
                            }
                            _ => false,
                        };
                        Some(if construction {
                            Type::ConfiguredProcess(owner)
                        } else {
                            Type::Run(owner)
                        })
                    }
                    Some(Type::Artifact(owner)) => Some(Type::Artifact(owner)),
                    _ => None,
                }
            }
            ExprKind::Number { suffix, value } => {
                if let Some(suffix) = suffix {
                    if let Some(id) = self.lookup(source, scope, suffix) {
                        self.refer(source, id, expr.span.clone(), suffix, true, false);
                    }
                }
                suffix.is_none().then(|| {
                    if value.contains('.') {
                        Type::Scalar
                    } else {
                        Type::Integer
                    }
                })
            }
            ExprKind::Bool { .. } => Some(Type::Bool),
            ExprKind::String { .. } => Some(Type::String),
            ExprKind::Binary { left, right, .. } => {
                self.expr(source, scope, left, false);
                self.expr(source, scope, right, false);
                None
            }
            ExprKind::Presentation { value, unit, .. } => {
                if let Some(unit) = unit {
                    self.expr(source, &HashMap::new(), unit, false);
                }
                self.expr(source, scope, value, false)
            }
            ExprKind::Unary { value, .. } => self.expr(source, scope, value, false),
            ExprKind::Array { items } => {
                let mut element = None;
                for value in items {
                    element = self.expr(source, scope, value, false);
                }
                element.map(|ty| Type::Array {
                    element: Box::new(ty),
                    size: Some(items.len().to_string()),
                })
            }
            ExprKind::NamedTuple { fields } => {
                let mut types = Vec::new();
                for field in fields {
                    let ty = self.expr(source, scope, &field.value, field.span == field.value.span);
                    if let (Some(name), Some(ty)) = (&field.name, ty) {
                        types.push((name.clone(), ty));
                    }
                }
                (types.len() == fields.len()).then_some(Type::NamedTuple(types))
            }
            ExprKind::Index { value, index, .. } => {
                let ty = self.expr(source, scope, value, false);
                if let Some(index) = index {
                    self.expr(source, scope, index, false);
                }
                match ty {
                    Some(Type::Array { element, .. } | Type::Stream(element)) => Some(*element),
                    _ => None,
                }
            }
            ExprKind::Lambda { parameters, body } => {
                let mut nested = scope.clone();
                for name in parameters {
                    if let Some(id) = self.add(
                        source,
                        name,
                        expr.span.start..body.span.start,
                        format!("{name}: lambda parameter"),
                        None,
                        None,
                        false,
                        expr.span.clone(),
                        None,
                    ) {
                        nested.insert(name.clone(), id);
                    }
                }
                self.expr(source, &nested, body, false);
                None
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{AnalysisHost, CompilerOptions};
    use std::fs;
    use tempfile::tempdir;

    fn snapshot(text: &str) -> (tempfile::TempDir, PathBuf, AnalysisSnapshot) {
        let dir = tempdir().unwrap();
        let path = dir.path().join("main.req");
        fs::write(&path, text).unwrap();
        let snap = AnalysisHost::new(CompilerOptions {
            absolute_paths: true,
            ..Default::default()
        })
        .snapshot(&path)
        .unwrap();
        (dir, path, snap)
    }

    #[test]
    fn local_rename_edits_only_bound_identifiers_not_comments_strings_or_other_processes() {
        let text = "process P { () => { let count = 2\n# count stays here\nlet text = \"count\"\nrequires count > 0\n} }\nprocess Q { () => { let count = 4\nrequires count > 0\n} }";
        let (_dir, path, snap) = snapshot(text);
        let offset = text.find("count =").unwrap();
        let edits = snap.rename_at(&path, offset, "sample_count").unwrap();
        assert_eq!(edits.len(), 2, "{edits:?}");
        assert!(
            edits
                .iter()
                .all(|edit| &text[edit.range.clone()] == "count")
        );
        assert!(
            edits
                .iter()
                .all(|edit| edit.range.end < text.find("process Q").unwrap())
        );
        assert!(
            snap.rename_at(&path, offset, "text")
                .unwrap_err()
                .contains("conflict")
        );
        assert!(snap.rename_at(&path, offset, "requires").is_err());
    }

    #[test]
    fn member_navigation_separates_identically_named_artifact_fields() {
        let text = "artifact A { size: Length }\nartifact B { size: Mass }\nprocess P { gets a: A\ngets b: B\n() => { requires a.size > 0 Meter\nrequires b.size > 0 Kilogram\n} }";
        let (_dir, path, snap) = snapshot(text);
        let offset = text.find("a.size").unwrap() + 2;
        let definition = snap.definition_at(&path, offset).unwrap();
        assert_eq!(definition.range.start, text.find("size:").unwrap());
        assert_eq!(snap.references_at(&path, offset, true).len(), 2);
        assert!(
            snap.hover_at(&path, offset)
                .unwrap()
                .markdown
                .contains("Length")
        );
        assert!(snap.rename_at(&path, offset, "width").is_err());
    }

    #[test]
    fn namespace_definition_targets_original_unsaved_source() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("main.req");
        let lib = dir.path().join("module.req");
        fs::write(&lib, "process Worker {}\n").unwrap();
        let text = "import \"module.req\" as module\nprocess P { () => { let configured = module.Worker()\nlet run = configured()\n} }";
        fs::write(&path, text).unwrap();
        let mut host = AnalysisHost::new(CompilerOptions {
            absolute_paths: true,
            ..Default::default()
        });
        let overlay = "# unsaved\nprocess Worker { config size: Integer = 4 }\n";
        host.set_file(&lib, overlay);
        let snap = host.snapshot(&path).unwrap();
        let definition = snap
            .definition_at(&path, text.find("module.Worker").unwrap() + 8)
            .unwrap();
        assert_eq!(definition.path, lib.canonicalize().unwrap());
        assert_eq!(&overlay[definition.range], "Worker");
        assert!(
            snap.hover_at(&path, text.find("module.Worker").unwrap() + 8)
                .unwrap()
                .markdown
                .contains("process Worker")
        );
    }

    #[test]
    fn inherited_member_navigation_keeps_the_declaring_contract_source() {
        let text = "process Base { config size: Integer = 2 }\nprocess Child: Base { () => { requires size > 0\n} }";
        let (_dir, path, snap) = snapshot(text);
        let offset = text.find("requires size").unwrap() + 9;
        let definition = snap.definition_at(&path, offset).unwrap();
        assert_eq!(definition.range.start, text.find("size:").unwrap());
        assert!(snap.rename_at(&path, offset, "width").is_err());
    }

    #[test]
    fn punned_argument_rename_is_rejected_without_changing_the_public_label() {
        let text = "process Child { config size: Integer\n() => {} }\nprocess P { () => { let size = 2\nlet child = Child(size)()\n} }";
        let (_dir, path, snap) = snapshot(text);
        assert!(
            snap.rename_at(&path, text.find("let size").unwrap() + 4, "new_size")
                .is_err()
        );
    }

    #[test]
    fn local_rename_rejects_unused_configuration_member_collision() {
        let text = "process P { config reserved: Integer = 3\n() => { let count = 2\nrequires count > 0\n} }";
        let (_dir, path, snap) = snapshot(text);
        let offset = text.find("let count").unwrap() + 4;
        let edits = snap.rename_at(&path, offset, "sample_count").unwrap();
        assert_eq!(edits.len(), 2);
        assert!(snap.rename_at(&path, offset, "reserved").is_err());
    }

    #[test]
    fn explanation_uses_compiler_predicate_and_origin() {
        let text = "process P { config size: Integer\n() => { requires size > 0\n} }";
        let (_dir, path, snap) = snapshot(text);
        let explanation = snap
            .explain_at(&path, text.find("size >").unwrap())
            .unwrap();
        assert!(
            explanation.markdown.contains("size > 0"),
            "{}",
            explanation.markdown
        );
        assert!(explanation.markdown.contains("Origin:"));
        assert!(explanation.markdown.contains("Entry:"));
    }

    #[test]
    fn hover_retains_refinement_and_instance_metric_context() {
        let text = "unit Positive = Scalar(x) { requires x > 0 }\nprocess P { config amount: Positive = 3\nexport total: Scalar\nmonitor total\n() => { total = amount } }";
        let (_dir, path, snap) = snapshot(text);
        let hover = snap
            .hover_at(&path, text.find("Positive =").unwrap())
            .unwrap();
        assert!(hover.markdown.contains("Refinement:"), "{}", hover.markdown);
        let total = snap.hover_at(&path, text.find("total:").unwrap()).unwrap();
        assert!(total.markdown.contains("P.total"), "{}", total.markdown);
        assert!(total.markdown.contains("entry"));
    }
}
