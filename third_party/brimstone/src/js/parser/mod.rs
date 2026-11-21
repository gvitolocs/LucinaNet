pub mod analyze;
pub mod ast;
pub mod ast_visitor;
mod context;
mod lexer;
pub mod lexer_stream;
pub mod loc;
mod parse_error;
mod parsed_program;
#[allow(clippy::module_inception)]
pub mod parser;
mod printer;
pub mod regexp;
pub mod regexp_parser;
pub mod reparse;
// proof text: a reparse mutant searches `i = i - 1;`; sites: src/js/parser/reparse_rules.rs:76
#[allow(clippy::assign_op_pattern)]
mod reparse_rules;
pub mod scope_tree;
pub mod source;
mod token;

pub use context::ParseContext;
pub use parse_error::{LocalizedParseError, LocalizedParseErrors, ParseError, ParseResult};
pub use parsed_program::ParsedProgram;
pub use parser::{parse_module, parse_script, parse_script_for_eval};
pub use printer::print_program;
