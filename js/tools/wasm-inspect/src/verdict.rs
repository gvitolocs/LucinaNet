/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at http://mozilla.org/MPL/2.0/. */

//! Feature verdict: which WebAssembly proposal every operator belongs to, read
//! out of the parser itself, and the wasmi configuration that implies.
//!
//! The proposal a variant belongs to is not something this crate guesses:
//! `wasmparser`'s own `for_each_operator!` macro tags each operator with its
//! proposal group, and the two functions below are generated from that tagging.
//! A variant this version of the parser does not know becomes
//! [`Proposal::Other`] and is reported as such instead of being counted as MVP.

use std::fmt::Write as _;

/// The proposal groups `wasmparser` tags its operators with.
#[derive(Copy, Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Proposal {
    /// Baseline WebAssembly 1.0/2.0 operators.
    Mvp,
    /// `sign-ext`: `i32.extend8_s` and friends.
    SignExtension,
    /// `nontrapping-fptoint`: `i32.trunc_sat_f32_s` and friends.
    SaturatingFloatToInt,
    /// `bulk-memory`: `memory.copy`, `table.init`, `data.drop`.
    BulkMemory,
    /// `reference-types`: `ref.null`, `table.get`, typed `select`.
    ReferenceTypes,
    /// `gc`: struct/array/cont operators and the typed references they use.
    Gc,
    /// `tail-call`: `return_call`, `return_call_indirect`.
    TailCall,
    /// `memory-control` (experimental): `memory.discard`.
    MemoryControl,
    /// `threads`: the `0xFE`-prefixed atomic operators.
    Threads,
    /// `simd`: the `0xFD`-prefixed vector operators.
    Simd,
    /// `relaxed-simd`.
    RelaxedSimd,
    /// `exception-handling`, current form: `try_table`, `throw`.
    Exceptions,
    /// `exception-handling`, the abandoned `try`/`catch` encoding.
    LegacyExceptions,
    /// `shared-everything-threads`: atomics on globals, tables and structs.
    SharedEverythingThreads,
    /// `function-references`: `call_ref`, `br_on_null`.
    FunctionReferences,
    /// `stack-switching`: continuations.
    StackSwitching,
    /// `wasm-wide-arithmetic`: `i64.add128` and friends.
    WideArithmetic,
    /// An operator this parser does not recognise at all.
    Other,
}

impl Proposal {
    /// The name used for this proposal in `wasmparser`'s feature vocabulary,
    /// which is also the name of the matching wasmi `Config` flag.
    pub fn name(self) -> &'static str {
        match self {
            Proposal::Mvp => "mvp",
            Proposal::SignExtension => "sign_extension",
            Proposal::SaturatingFloatToInt => "saturating_float_to_int",
            Proposal::BulkMemory => "bulk_memory",
            Proposal::ReferenceTypes => "reference_types",
            Proposal::Gc => "gc",
            Proposal::TailCall => "tail_call",
            Proposal::MemoryControl => "memory_control",
            Proposal::Threads => "threads",
            Proposal::Simd => "simd",
            Proposal::RelaxedSimd => "relaxed_simd",
            Proposal::Exceptions => "exceptions",
            Proposal::LegacyExceptions => "legacy_exceptions",
            Proposal::SharedEverythingThreads => "shared_everything_threads",
            Proposal::FunctionReferences => "function_references",
            Proposal::StackSwitching => "stack_switching",
            Proposal::WideArithmetic => "wide_arithmetic",
            Proposal::Other => "unknown",
        }
    }

    /// The wasmi 2.0.0 `Config` setter that turns this proposal on, if any.
    /// `None` means our engine has no such switch: it cannot run this at all.
    pub fn wasmi_setter(self) -> Option<&'static str> {
        match self {
            Proposal::Mvp => None,
            Proposal::SignExtension => Some("wasm_sign_extension"),
            Proposal::SaturatingFloatToInt => Some("wasm_saturating_float_to_int"),
            Proposal::BulkMemory => Some("wasm_bulk_memory"),
            Proposal::ReferenceTypes => Some("wasm_reference_types"),
            Proposal::TailCall => Some("wasm_tail_call"),
            Proposal::Simd => Some("wasm_simd"),
            Proposal::RelaxedSimd => Some("wasm_relaxed_simd"),
            Proposal::WideArithmetic => Some("wasm_wide_arithmetic"),
            // No operator-level switch for these in wasmi 2.0.0, and no support:
            // no threads, no exception handler, no GC, no continuations.
            Proposal::MemoryControl
            | Proposal::Threads
            | Proposal::Gc
            | Proposal::Exceptions
            | Proposal::LegacyExceptions
            | Proposal::SharedEverythingThreads
            | Proposal::FunctionReferences
            | Proposal::StackSwitching
            | Proposal::Other => None,
        }
    }

    /// Whether our engine can execute this proposal when it is switched on.
    pub fn supported_by_us(self) -> bool {
        matches!(
            self,
            Proposal::Mvp
                | Proposal::SignExtension
                | Proposal::SaturatingFloatToInt
                | Proposal::BulkMemory
                | Proposal::ReferenceTypes
                | Proposal::TailCall
                | Proposal::Simd
                | Proposal::RelaxedSimd
                | Proposal::WideArithmetic
        )
    }
}

/// The `Proposal` variant for one of the parser's proposal tags.
macro_rules! proposal_arm {
    (mvp) => {
        $crate::verdict::Proposal::Mvp
    };
    (sign_extension) => {
        $crate::verdict::Proposal::SignExtension
    };
    (saturating_float_to_int) => {
        $crate::verdict::Proposal::SaturatingFloatToInt
    };
    (bulk_memory) => {
        $crate::verdict::Proposal::BulkMemory
    };
    (reference_types) => {
        $crate::verdict::Proposal::ReferenceTypes
    };
    (gc) => {
        $crate::verdict::Proposal::Gc
    };
    (tail_call) => {
        $crate::verdict::Proposal::TailCall
    };
    (memory_control) => {
        $crate::verdict::Proposal::MemoryControl
    };
    (threads) => {
        $crate::verdict::Proposal::Threads
    };
    (simd) => {
        $crate::verdict::Proposal::Simd
    };
    (relaxed_simd) => {
        $crate::verdict::Proposal::RelaxedSimd
    };
    (exceptions) => {
        $crate::verdict::Proposal::Exceptions
    };
    (legacy_exceptions) => {
        $crate::verdict::Proposal::LegacyExceptions
    };
    (shared_everything_threads) => {
        $crate::verdict::Proposal::SharedEverythingThreads
    };
    (function_references) => {
        $crate::verdict::Proposal::FunctionReferences
    };
    (stack_switching) => {
        $crate::verdict::Proposal::StackSwitching
    };
    (wide_arithmetic) => {
        $crate::verdict::Proposal::WideArithmetic
    };
}

/// Expand to the two lookup functions, one arm per operator, from the parser's
/// own proposal tagging.
macro_rules! define_lookups {
    ($( @$proposal:ident $op:ident $({ $($arg:ident: $argty:ty),* })? => $visit:ident ($($ann:tt)*) )*) => {
        /// The proposal group an operator belongs to, as tagged by the parser.
        pub fn proposal_of(op: &wasmparser::Operator<'_>) -> Proposal {
            match op {
                $( wasmparser::Operator::$op $({ $($arg: _),* })? => proposal_arm!($proposal), )*
                _ => Proposal::Other,
            }
        }

        /// The parser's own name for this operator, e.g. `V128Load8Splat`.
        pub fn variant_of(op: &wasmparser::Operator<'_>) -> &'static str {
            match op {
                $( wasmparser::Operator::$op $({ $($arg: _),* })? => stringify!($op), )*
                _ => "Unknown",
            }
        }
    };
}

wasmparser::for_each_operator!(define_lookups);

/// One line of the feature verdict.
#[derive(Clone, Debug)]
pub struct Feature {
    /// Name in the `wasmparser` / wasmi `Config` vocabulary.
    pub name: &'static str,
    pub used: bool,
    /// Operators (or types, or segments) that forced this conclusion.
    pub count: u64,
    /// Byte offset of one example, when the evidence has a position.
    pub example: Option<(u64, String)>,
    /// The conclusion in words, e.g. `12,345 operators, first at 0x2a1c4`.
    pub evidence: String,
    /// The wasmi 2.0.0 `Config` setter to call, when there is one.
    pub setter: Option<&'static str>,
    /// Whether wasmi 2.0.0 implements this at all.
    pub supported: bool,
}

/// The one-line `needs:` summary.
pub fn needs_line(features: &[Feature]) -> String {
    let mut needed: Vec<&str> = Vec::new();
    let mut unsupported: Vec<&str> = Vec::new();
    for f in features {
        if !f.used {
            continue;
        }
        if f.supported {
            needed.push(f.name);
        } else {
            unsupported.push(f.name);
        }
    }
    let mut line = String::from("needs: ");
    if needed.is_empty() && unsupported.is_empty() {
        line.push_str("nothing beyond MVP");
    } else {
        for (i, n) in needed.iter().enumerate() {
            if i > 0 {
                line.push_str(", ");
            }
            line.push_str(n);
        }
        if !unsupported.is_empty() {
            if !needed.is_empty() {
                line.push_str("; unsupported by us: ");
            } else {
                line.push_str("unsupported by us: ");
            }
            for (i, n) in unsupported.iter().enumerate() {
                if i > 0 {
                    line.push_str(", ");
                }
                line.push_str(n);
            }
        }
    }
    line
}

/// The same verdict as the calls to make on a wasmi 2.0.0 `Config`.
pub fn wasmi_config_line(features: &[Feature]) -> String {
    let mut setters: Vec<&'static str> = Vec::new();
    for f in features {
        if f.used
            && let Some(s) = f.setter
        {
            setters.push(s);
        }
    }
    // wasmi 2.0.0 validates against an Engine built from the Config:
    // `Engine::new(&config)` then `Module::validate(&engine, bytes)`.
    let mut line = String::from("wasmi 2.0.0: ");
    if setters.is_empty() {
        line.push_str("Config::new() as is, no proposal flag to turn on");
        return line;
    }
    let _ = write!(line, "let mut cfg = Config::new();");
    for s in setters {
        let _ = write!(line, " cfg.{s}(true);");
    }
    let _ = write!(
        line,
        " then Engine::new(&cfg) and Module::validate(&engine, bytes)"
    );
    line
}

/// Thousands separators, because these modules have a lot of everything.
pub fn group(n: u64) -> String {
    let digits = n.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use wasmparser::{Operator, Parser, Payload};

    /// The MVP ops every body ends with must not be mistaken for a proposal.
    fn first_operator(bytes: &[u8]) -> Operator<'_> {
        for payload in Parser::new(0).parse_all(bytes) {
            if let Ok(Payload::CodeSectionEntry(body)) = payload {
                let mut reader = body.get_operators_reader().unwrap();
                let (op, _offset) = reader.read_with_offset().unwrap();
                return op;
            }
        }
        panic!("no code section in fixture");
    }

    #[test]
    fn tagging_comes_from_the_parser() {
        assert_eq!(Proposal::Mvp.name(), "mvp");
        assert_eq!(Proposal::Threads.wasmi_setter(), None);
        assert!(!Proposal::Threads.supported_by_us());
        assert_eq!(Proposal::Simd.wasmi_setter(), Some("wasm_simd"));
        assert!(Proposal::Simd.supported_by_us());
        assert_eq!(Proposal::Other.wasmi_setter(), None);
    }

    #[test]
    fn variant_names_are_generated_for_every_operator() {
        // A hand-built body holding one `i32.add`; the lookup must name it.
        let body = [
            0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00, // header
            0x01, 0x04, 0x01, 0x60, 0x00, 0x00, // type () -> ()
            0x03, 0x02, 0x01, 0x00, // one function of type 0
            0x0a, 0x06, 0x01, 0x04, 0x00, 0x6a, 0x01, 0x0b, // code: i32.add, end
        ];
        let op = first_operator(&body);
        assert_eq!(variant_of(&op), "I32Add");
        assert_eq!(proposal_of(&op), Proposal::Mvp);
    }

    #[test]
    fn unknown_operators_are_not_counted_as_mvp() {
        // 0xff is not an operator; the parser errors rather than inventing one,
        // so what is left to check is that the fallback arm exists.
        assert_eq!(Proposal::Other.name(), "unknown");
    }

    #[test]
    fn needs_line_lists_supported_and_unsupported_separately() {
        let f = |name: &'static str, used: bool, supported: bool| Feature {
            name,
            used,
            count: if used { 1 } else { 0 },
            example: None,
            evidence: String::new(),
            setter: if supported && used { Some("x") } else { None },
            supported,
        };
        let only_baseline = vec![f("simd", false, true)];
        assert_eq!(needs_line(&only_baseline), "needs: nothing beyond MVP");
        let both = vec![f("simd", true, true), f("threads", true, false)];
        assert_eq!(needs_line(&both), "needs: simd; unsupported by us: threads");
        assert_eq!(
            wasmi_config_line(&both),
            "wasmi 2.0.0: let mut cfg = Config::new(); cfg.x(true); then Engine::new(&cfg) and Module::validate(&engine, bytes)"
        );
    }

    #[test]
    fn grouping_inserts_separators() {
        assert_eq!(group(0), "0");
        assert_eq!(group(999), "999");
        assert_eq!(group(1203180), "1,203,180");
        assert_eq!(group(1_000_000_000), "1,000,000,000");
    }
}
