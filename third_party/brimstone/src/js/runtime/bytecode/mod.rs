pub mod cache;
pub mod call_position;
#[cfg(test)]
mod call_position_tests;
pub mod constant_table;
mod constant_table_builder;
pub mod exception_handlers;
pub mod fncache;
pub mod function;
pub mod generator;
pub mod graphviz;
pub mod instruction;
mod instruction_traits;
pub mod lazy;
mod operand;
mod register_allocator;
pub mod source_map;
pub mod stack_frame;
pub mod vm;
mod width;
mod writer;

pub use operand::Register;
pub use width::ExtraWide;
