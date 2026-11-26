//! Turning a caught panic into a defect.

use std::any::Any;

use roadbook_core::Defect;

/// The defect a panic with payload `p` stands for: a [`Defect`] payload unchanged (what a
/// run that ended with a defect raises), a `&str` or `String` payload as a host defect with
/// that message, and anything else as a host defect with the message `Box<dyn Any>`, which
/// is what Rust's panic hook prints for one.
pub(crate) fn defect_from_panic(p: Box<dyn Any + Send>) -> Defect {
    let p = match p.downcast::<Defect>() {
        Ok(d) => return *d,
        Err(p) => p,
    };
    let p = match p.downcast::<&'static str>() {
        Ok(s) => return Defect::host(*s),
        Err(p) => p,
    };
    match p.downcast::<String>() {
        Ok(s) => Defect::host(*s),
        Err(_) => Defect::host("Box<dyn Any>"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn payloads() {
        let d = Defect::Limit;
        assert_eq!(defect_from_panic(Box::new(d.clone())), d);
        assert_eq!(defect_from_panic(Box::new("boom")), Defect::host("boom"));
        assert_eq!(
            defect_from_panic(Box::new(String::from("bang"))),
            Defect::host("bang")
        );
        assert_eq!(
            defect_from_panic(Box::new(7_u8)),
            Defect::host("Box<dyn Any>")
        );
    }
}
