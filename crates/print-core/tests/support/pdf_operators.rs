// Inspect actual ExtGState definitions and their resource operands, so tests
// constrain compositing without depending on the writer's resource-name scheme.
pub fn alpha_invocation_count(pdf: &str, expected: f32) -> usize {
    let references = pdf
        .split("endobj")
        .filter_map(|object| {
            if !object.contains("/Type /ExtGState") {
                return None;
            }
            let matches_alpha = object
                .lines()
                .filter_map(|line| {
                    line.trim()
                        .strip_prefix("/ca ")
                        .or_else(|| line.trim().strip_prefix("/CA "))
                })
                .filter_map(|value| value.parse::<f32>().ok())
                .any(|value| (value - expected).abs() <= 0.000_001);
            if !matches_alpha {
                return None;
            }
            let header = object.lines().find(|line| line.ends_with(" 0 obj"))?;
            Some(header.trim().split_ascii_whitespace().next()?.to_owned())
        })
        .collect::<Vec<_>>();
    let mut names = std::collections::BTreeSet::new();
    for line in pdf.lines() {
        let tokens = line.split_ascii_whitespace().collect::<Vec<_>>();
        if tokens.len() == 4
            && tokens[0].starts_with('/')
            && tokens[2] == "0"
            && tokens[3] == "R"
            && references.iter().any(|reference| reference == tokens[1])
        {
            names.insert(tokens[0].to_owned());
        }
    }
    names
        .iter()
        .map(|name| pdf.matches(&format!("{name} gs")).count())
        .sum()
}
