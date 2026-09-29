//! Types as the partial evaluator needs them: resolved aliases, struct and
//! enum definitions by name, default values.

use super::val::IT;
use crate::rs::ast::{Item, Ty};
use crate::rs::parse::Program;
use std::collections::BTreeMap;
use std::rc::Rc;

#[derive(Clone, Debug, PartialEq)]
pub enum RTy {
    Int(IT),
    Bool,
    F64,
    F32,
    Unit,
    Str,
    Tuple(Vec<RTy>),
    /// Structs, enums, Option, Result, Tup, Stk (by short name).
    Named(Rc<str>, Vec<RTy>),
    Ref(Box<RTy>),
    Array(Box<RTy>),
    Fn,
    Infer,
}

#[derive(Clone, Debug)]
pub enum Def {
    Struct(Vec<(String, Ty)>, bool),
    Enum(Vec<(String, Vec<Ty>)>),
}

pub struct Types {
    /// Short name -> (full path, definition).
    pub defs: BTreeMap<String, (String, Def)>,
    pub aliases: BTreeMap<String, Ty>,
}

impl Types {
    pub fn new(p: &Program) -> Types {
        let mut paths: Vec<&String> = p.items.keys().collect();
        paths.sort();
        let mut defs = BTreeMap::new();
        let mut aliases = BTreeMap::new();
        for path in paths {
            let short = path.rsplit("::").next().unwrap().to_string();
            match &p.items[path] {
                Item::Struct(_, f, tuple) => {
                    defs.entry(short).or_insert((path.clone(), Def::Struct(f.clone(), *tuple)));
                }
                Item::Enum(_, v) => {
                    defs.entry(short).or_insert((path.clone(), Def::Enum(v.clone())));
                }
                Item::TypeAlias(_, t) => {
                    aliases.entry(short).or_insert(t.clone());
                }
                _ => {}
            }
        }
        Types { defs, aliases }
    }

    pub fn resolve(&self, t: &Ty) -> RTy {
        match t {
            Ty::Tuple(v) => {
                if v.is_empty() {
                    RTy::Unit
                } else {
                    RTy::Tuple(v.iter().map(|x| self.resolve(x)).collect())
                }
            }
            Ty::Ref(_, x) => RTy::Ref(Box::new(self.resolve(x))),
            Ty::Array(x, _) | Ty::Slice(x) => RTy::Array(Box::new(self.resolve(x))),
            Ty::Fn => RTy::Fn,
            Ty::Infer => RTy::Infer,
            Ty::Path(p, args) => {
                let name = p.rsplit("::").next().unwrap();
                if let Some(it) = IT::from_name(name) {
                    return RTy::Int(it);
                }
                match name {
                    "bool" => return RTy::Bool,
                    "f64" => return RTy::F64,
                    "f32" => return RTy::F32,
                    "str" => return RTy::Str,
                    "R" => {
                        let inner = args.first().map(|a| self.resolve(a)).unwrap_or(RTy::Unit);
                        return RTy::Named("Result".into(), vec![inner, RTy::Named("Trap".into(), vec![])]);
                    }
                    _ => {}
                }
                if args.is_empty()
                    && let Some(a) = self.aliases.get(name)
                {
                    return self.resolve(a);
                }
                RTy::Named(name.into(), args.iter().map(|a| self.resolve(a)).collect())
            }
        }
    }

    pub fn struct_fields(&self, name: &str) -> Option<&Vec<(String, Ty)>> {
        match self.defs.get(name) {
            Some((_, Def::Struct(f, _))) => Some(f),
            _ => None,
        }
    }

    pub fn field_index(&self, name: &str, field: &str) -> Option<usize> {
        self.struct_fields(name)?.iter().position(|(f, _)| f == field)
    }

    pub fn variants(&self, name: &str) -> Option<&Vec<(String, Vec<Ty>)>> {
        match self.defs.get(name) {
            Some((_, Def::Enum(v))) => Some(v),
            _ => None,
        }
    }

    pub fn variant_index(&self, name: &str, var: &str) -> Option<u32> {
        match name {
            "Option" => {
                return match var {
                    "None" => Some(0),
                    "Some" => Some(1),
                    _ => None,
                };
            }
            "Result" => {
                return match var {
                    "Ok" => Some(0),
                    "Err" => Some(1),
                    _ => None,
                };
            }
            _ => {}
        }
        self.variants(name)?.iter().position(|(v, _)| v == var).map(|k| k as u32)
    }

    pub fn full_path(&self, name: &str) -> Option<&str> {
        self.defs.get(name).map(|(p, _)| p.as_str())
    }
}
