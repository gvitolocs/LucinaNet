//! LucinaNet on Blitz. See `lucinanet_ui::main` for the command line.

mod engine;

fn main() {
    lucinanet_ui::main(engine::start)
}
