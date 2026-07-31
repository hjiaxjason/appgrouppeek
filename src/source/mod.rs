//! Reading container contents.
//!
//! Everything here operates on a directory tree and knows nothing about
//! simulators, which is what makes it testable against a fixture directory with
//! nothing booted.
//!
//! There is deliberately no `ContainerSource` trait yet. A second implementation
//! (devicectl) is what will reveal the right trait shape; extracting it now would
//! be guessing.

pub mod sim;
