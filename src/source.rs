use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use indexmap::IndexMap;
use thiserror::Error;

use crate::ast::{ImportPath, Item};
use crate::parser::{Parse, parse};

#[derive(Debug, Error)]
pub enum SourceError {
    #[error("cannot read {path}: {source}")]
    Io {
        path: String,
        source: std::io::Error,
    },
    #[error("import cycle: {0}")]
    ImportCycle(String),
    #[error("unknown standard module {0}")]
    UnknownStandardModule(String),
}

#[derive(Clone, Debug)]
pub struct SourceFile {
    pub key: String,
    pub path: Option<PathBuf>,
    pub display_path: String,
    pub text: String,
    pub parse: Parse,
}

#[derive(Clone, Debug)]
pub struct SourceSet {
    pub entry_key: String,
    pub files: IndexMap<String, SourceFile>,
}

/// Resolve a file identity even when the editor has not saved the file yet.
/// Existing parents are canonicalized, retaining symlink identity and `..`.
pub(crate) fn source_path(path: &Path) -> std::io::Result<PathBuf> {
    match path.canonicalize() {
        Ok(path) => Ok(path),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let absolute = if path.is_absolute() {
                path.to_path_buf()
            } else {
                std::env::current_dir()?.join(path)
            };
            let parent = absolute.parent().ok_or(error)?;
            let name = absolute.file_name().ok_or_else(|| {
                std::io::Error::new(std::io::ErrorKind::NotFound, "missing file name")
            })?;
            Ok(source_path(parent)?.join(name))
        }
        Err(error) => Err(error),
    }
}

pub fn load(entry: &Path, absolute_paths: bool) -> Result<SourceSet, SourceError> {
    load_with_overlays(entry, absolute_paths, &HashMap::new())
}

pub fn load_with_overlays(
    entry: &Path,
    absolute_paths: bool,
    overlays: &HashMap<PathBuf, String>,
) -> Result<SourceSet, SourceError> {
    let canonical = source_path(entry).map_err(|source| SourceError::Io {
        path: entry.display().to_string(),
        source,
    })?;
    let root = canonical.parent().unwrap_or(Path::new(".")).to_path_buf();
    let entry_key = canonical.to_string_lossy().into_owned();
    let mut loader = Loader {
        root,
        absolute_paths,
        files: IndexMap::new(),
        stack: Vec::new(),
        overlays: overlays
            .iter()
            .filter_map(|(path, text)| {
                source_path(path)
                    .ok()
                    .map(|path| (path.to_string_lossy().into_owned(), text.clone()))
            })
            .collect(),
    };
    loader.load_disk(&canonical)?;
    Ok(SourceSet {
        entry_key,
        files: loader.files,
    })
}

/// Load an in-memory entry module whose quoted imports resolve from `base`.
///
/// Interactive tooling uses this instead of creating a scratch `.req` file in
/// the user's project. Imported files are otherwise loaded exactly as they are
/// for an ordinary compiler entry point, including cycle detection and
/// embedded `std.*` modules.
pub(crate) fn load_virtual(
    base: &Path,
    text: &str,
    absolute_paths: bool,
) -> Result<SourceSet, SourceError> {
    let root = base.canonicalize().map_err(|source| SourceError::Io {
        path: base.display().to_string(),
        source,
    })?;
    let virtual_path = root.join("__freql_calculator__.req");
    let entry_key = format!("virtual:{}", virtual_path.display());
    let parsed = parse(text);
    let entry_imports = imports(&parsed);
    let mut loader = Loader {
        root,
        absolute_paths,
        files: IndexMap::new(),
        stack: vec![entry_key.clone()],
        overlays: HashMap::new(),
    };
    loader.files.insert(
        entry_key.clone(),
        SourceFile {
            key: entry_key.clone(),
            path: Some(virtual_path.clone()),
            display_path: "<calculator>".into(),
            text: text.to_owned(),
            parse: parsed,
        },
    );
    for import in entry_imports {
        match import {
            ImportPath::Relative(relative) => {
                loader.load_disk(
                    &virtual_path
                        .parent()
                        .unwrap_or(Path::new("."))
                        .join(relative),
                )?;
            }
            ImportPath::Module(module) if module == "math" => {}
            ImportPath::Module(module) if module.starts_with("std.") => {
                loader.load_standard(&module)?;
            }
            ImportPath::Module(_) => {}
        }
    }
    loader.leave(&entry_key);
    Ok(SourceSet {
        entry_key,
        files: loader.files,
    })
}

struct Loader {
    root: PathBuf,
    absolute_paths: bool,
    files: IndexMap<String, SourceFile>,
    stack: Vec<String>,
    overlays: HashMap<String, String>,
}

impl Loader {
    fn load_disk(&mut self, path: &Path) -> Result<(), SourceError> {
        let canonical = source_path(path).map_err(|source| SourceError::Io {
            path: path.display().to_string(),
            source,
        })?;
        let key = canonical.to_string_lossy().into_owned();
        if self.stack.iter().any(|active| active == &key) {
            self.enter(&key)?;
        }
        if self.files.contains_key(&key) {
            return Ok(());
        }
        self.enter(&key)?;
        let text = if let Some(overlay) = self.overlays.get(&key) {
            overlay.clone()
        } else {
            fs::read_to_string(&canonical).map_err(|source| SourceError::Io {
                path: canonical.display().to_string(),
                source,
            })?
        };
        let parsed = parse(&text);
        let display_path = if self.absolute_paths {
            canonical.display().to_string()
        } else {
            canonical
                .strip_prefix(&self.root)
                .unwrap_or(&canonical)
                .to_string_lossy()
                .replace('\\', "/")
        };
        let imports = imports(&parsed);
        self.files.insert(
            key.clone(),
            SourceFile {
                key: key.clone(),
                path: Some(canonical.clone()),
                display_path,
                text,
                parse: parsed,
            },
        );
        for import in imports {
            match import {
                ImportPath::Relative(relative) => {
                    let imported = canonical.parent().unwrap_or(Path::new(".")).join(relative);
                    self.load_disk(&imported)?;
                }
                ImportPath::Module(module) if module == "math" => {}
                ImportPath::Module(module) if module.starts_with("std.") => {
                    self.load_standard(&module)?;
                }
                ImportPath::Module(_) => {}
            }
        }
        self.leave(&key);
        Ok(())
    }

    fn load_standard(&mut self, module: &str) -> Result<(), SourceError> {
        let key = format!("std:{module}");
        if self.stack.iter().any(|active| active == &key) {
            self.enter(&key)?;
        }
        if self.files.contains_key(&key) {
            return Ok(());
        }
        self.enter(&key)?;
        let text = match module {
            "std.process" => include_str!("../stdlib/std/process.req"),
            "std.collection" => include_str!("../stdlib/std/collection.req"),
            "std.stream" => include_str!("../stdlib/std/stream.req"),
            _ => return Err(SourceError::UnknownStandardModule(module.into())),
        }
        .to_owned();
        let parsed = parse(&text);
        let imports = imports(&parsed);
        self.files.insert(
            key.clone(),
            SourceFile {
                key: key.clone(),
                path: None,
                display_path: format!("<{}>", module.replace('.', "/")),
                text,
                parse: parsed,
            },
        );
        for import in imports {
            if let ImportPath::Module(module) = import
                && module.starts_with("std.")
            {
                self.load_standard(&module)?;
            }
        }
        self.leave(&key);
        Ok(())
    }

    fn enter(&mut self, key: &str) -> Result<(), SourceError> {
        if let Some(index) = self.stack.iter().position(|item| item == key) {
            let mut cycle = self.stack[index..].to_vec();
            cycle.push(key.to_owned());
            return Err(SourceError::ImportCycle(cycle.join(" -> ")));
        }
        self.stack.push(key.to_owned());
        Ok(())
    }

    fn leave(&mut self, key: &str) {
        if self.stack.last().is_some_and(|last| last == key) {
            self.stack.pop();
        }
    }
}

fn imports(parsed: &Parse) -> Vec<ImportPath> {
    parsed
        .module
        .items
        .iter()
        .filter_map(|item| match item {
            Item::Import(import) => Some(import.path.clone()),
            _ => None,
        })
        .collect()
}

pub fn line_column(source: &str, offset: usize) -> (usize, usize) {
    let prefix = &source[..offset.min(source.len())];
    let line = prefix.bytes().filter(|byte| *byte == b'\n').count() + 1;
    let column = prefix
        .rsplit_once('\n')
        .map(|(_, tail)| tail.chars().count() + 1)
        .unwrap_or_else(|| prefix.chars().count() + 1);
    (line, column)
}
