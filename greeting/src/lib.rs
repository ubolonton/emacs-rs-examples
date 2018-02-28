extern crate libc;
#[macro_use]
extern crate emacs;

use emacs::{Env, Result, Value};

// Emacs won't load the module without this.
emacs_plugin_is_GPL_compatible!();

// Declare and define the init function, which Emacs will call when it loads the module.
emacs_module_init!(init);
fn init(env: &Env) -> Result<Value> {
    env.message("Hello, Emacs!")?;
    env.provide("greeting")
}
