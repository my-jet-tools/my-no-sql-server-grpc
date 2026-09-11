fn main() {
    ci_utils::css::CssCompiler::new("./css")
        .add_file("01-tokens.css")
        .add_file("02-shell.css")
        .add_file("03-atoms.css")
        .add_file("04-overview.css")
        .add_file("05-data.css")
        .add_file("06-connections.css")
        .add_file("07-mini-chart.css")
        .compile("./public/assets/app.css");
}
