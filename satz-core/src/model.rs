pub mod document;
pub mod footnote;
pub mod frontmatter;
pub mod heading;
pub mod link;
pub mod range;
pub mod tag;

pub use document::{DocId, Document};
pub use footnote::{FootnoteDef, FootnoteTable};
pub use frontmatter::Frontmatter;
pub use heading::Heading;
pub use link::{Link, LinkKind};
pub use range::ByteRange;
pub use tag::Tag;
