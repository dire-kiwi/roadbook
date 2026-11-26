// A `Handlers` implementation without one of the chart's methods does not compile.

roadbook::include!("charts/small.road");

struct App;

impl small::Handlers for App {
    fn greeting(&self, name: &str) -> String {
        format!("hello, {name}")
    }
}

fn main() {}
