mod index;
mod schema;
mod search;
mod tokenizer;

use magnus::{define_module, Error};

#[magnus::init]
fn init() -> Result<(), Error> {
    let module = define_module("TantivyRb")?;
    schema::init(module)?;
    index::init(module)?;
    Ok(())
}
