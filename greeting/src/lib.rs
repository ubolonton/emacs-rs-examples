use emacs::{Env, Result};

// Emacs won't load the module without this.
emacs::plugin_is_GPL_compatible!();

// Declare and define the init function, which Emacs will call when it loads the module.
#[emacs::module(name(fn))]
fn greeting(env: &Env) -> Result<()> {
    env.message("Hello, Emacs!")?;
    Ok(())
}
