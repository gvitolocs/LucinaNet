//! LucinaNet on Servo, painted in software by swgl. See `lucinanet_ui::main` for the command line.

mod css_prefs;
mod doors;
mod domains;
mod engine;
mod errlog_glue;
#[cfg(feature = "media")]
mod media_images;
mod marks;
mod pagetext;
mod persist;
#[cfg(feature = "harness")]
mod probes;
mod rules;
mod soft;
mod visits;
mod zoom;

/// jemalloc for all the program, C++ included: musl's malloc is slow across threads and keeps what it gets.
/// One arena per core would hold memory for little gain, and freed pages go back within seconds.
#[unsafe(no_mangle)]
pub static malloc_conf: JemallocConf = JemallocConf(c"narenas:4,dirty_decay_ms:3000,muzzy_decay_ms:1000".as_ptr());

#[repr(transparent)]
pub struct JemallocConf(*const std::ffi::c_char);

// SAFETY: it points at a string constant.
unsafe impl Sync for JemallocConf {}

fn main() {
    lucinanet_ui::phase("main");
    engine::set_js_fn_cache_dir();
    rustls_graviola::default_provider()
        .install_default()
        .expect("a crypto provider");
    // The diagnostic subcommands are in a `harness` build only: the shipped binary has none of them (probes.rs).
    #[cfg(feature = "harness")]
    {
        let args: Vec<String> = std::env::args().collect();
        if probes::dispatch(&args) {
            return;
        }
    }
    lucinanet_ui::main(engine::start)
}

