//! LaTeX to readable Unicode: the formula renderer.
//!
//! A note's maths arrives as LaTeX source — `$x^2$`, `$$\frac{a}{b}$$`,
//! `\(\alpha\)` — and the app has no math typesetting engine (and will not
//! grow one). What it can do is show the formula the way a person reads it:
//! every common command mapped to its Unicode character, superscripts and
//! subscripts as raised and lowered glyphs, fractions and roots in a form
//! that survives a plain-text line.
//!
//! Two outputs, one source:
//! - [`to_unicode`] — the visual form, for the preview: `x² + √(y)`.
//! - [`to_speech`] — the spoken form, for the voice: "x squared plus the
//!   square root of y".
//!
//! Nothing is dropped: an unknown command keeps its name (`\foo` stays
//! `\foo`), so a formula never silently turns into nonsense.

/// The Greek letters and the symbols a note actually uses.
const SYMBOLS: &[(&str, &str)] = &[
    // Lowercase Greek.
    ("alpha", "α"), ("beta", "β"), ("gamma", "γ"), ("delta", "δ"),
    ("epsilon", "ε"), ("zeta", "ζ"), ("eta", "η"), ("theta", "θ"),
    ("iota", "ι"), ("kappa", "κ"), ("lambda", "λ"), ("mu", "μ"),
    ("nu", "ν"), ("xi", "ξ"), ("omicron", "ο"), ("pi", "π"),
    ("rho", "ρ"), ("sigma", "σ"), ("tau", "τ"), ("upsilon", "υ"),
    ("phi", "φ"), ("chi", "χ"), ("psi", "ψ"), ("omega", "ω"),
    // Uppercase Greek.
    ("Gamma", "Γ"), ("Delta", "Δ"), ("Theta", "Θ"), ("Lambda", "Λ"),
    ("Xi", "Ξ"), ("Pi", "Π"), ("Sigma", "Σ"), ("Upsilon", "Υ"),
    ("Phi", "Φ"), ("Psi", "Ψ"), ("Omega", "Ω"),
    // Operators and relations.
    ("times", "×"), ("div", "÷"), ("pm", "±"), ("mp", "∓"),
    ("cdot", "·"), ("leq", "≤"), ("le", "≤"), ("geq", "≥"),
    ("ge", "≥"), ("neq", "≠"), ("ne", "≠"), ("approx", "≈"),
    ("equiv", "≡"), ("sim", "∼"), ("propto", "∝"),
    ("in", "∈"), ("notin", "∉"), ("subset", "⊂"), ("subseteq", "⊆"),
    ("supset", "⊃"), ("supseteq", "⊇"), ("cup", "∪"), ("cap", "∩"),
    ("emptyset", "∅"), ("forall", "∀"), ("exists", "∃"),
    ("neg", "¬"), ("land", "∧"), ("lor", "∨"),
    // Arrows.
    ("rightarrow", "→"), ("to", "→"), ("leftarrow", "←"),
    ("leftrightarrow", "↔"), ("Rightarrow", "⇒"), ("Leftarrow", "⇐"),
    ("Leftrightarrow", "⇔"), ("mapsto", "↦"),
    // Misc.
    ("infty", "∞"), ("partial", "∂"), ("nabla", "∇"), ("cdot", "·"),
    ("ldots", "…"), ("dots", "…"), ("cdots", "⋯"), ("vdots", "⋮"),
    ("ddots", "⋱"), ("prime", "′"), ("deg", "°"), ("circ", "°"),
    ("ell", "ℓ"), ("hbar", "ℏ"), ("Re", "ℜ"), ("Im", "ℑ"),
    ("aleph", "ℵ"), ("wp", "℘"), ("imath", "ı"), ("jmath", "ȷ"),
    ("sum", "∑"), ("prod", "∏"), ("int", "∫"), ("oint", "∮"),
    ("sqrt", "√"), ("angle", "∠"), ("perp", "⊥"), ("parallel", "∥"),
    ("therefore", "∴"), ("because", "∵"), ("star", "⋆"),
    ("bullet", "•"), ("oplus", "⊕"), ("otimes", "⊗"),
    ("dagger", "†"), ("ddagger", "‡"), ("langle", "⟨"),
    ("rangle", "⟩"), ("lfloor", "⌊"), ("rfloor", "⌋"),
    ("lceil", "⌈"), ("rceil", "⌉"), ("checkmark", "✓"),
    // Text-mode helpers that carry no symbol but must not show a backslash.
    ("text", ""), ("mathrm", ""), ("mathbf", ""), ("mathit", ""),
    ("mathbb", ""), ("mathcal", ""), ("mathsf", ""), ("mathtt", ""),
    ("boldsymbol", ""), ("operatorname", ""), ("textbf", ""),
    ("textit", ""), ("mathtt", ""),
];

/// The digits and letters that have raised and lowered forms.
fn superscript(c: char) -> Option<char> {
    Some(match c {
        '0' => '⁰', '1' => '¹', '2' => '²', '3' => '³', '4' => '⁴',
        '5' => '⁵', '6' => '⁶', '7' => '⁷', '8' => '⁸', '9' => '⁹',
        'a' => 'ᵃ', 'b' => 'ᵇ', 'c' => 'ᶜ', 'd' => 'ᵈ', 'e' => 'ᵉ',
        'f' => 'ᶠ', 'g' => 'ᵍ', 'h' => 'ʰ', 'i' => 'ⁱ', 'j' => 'ʲ',
        'k' => 'ᵏ', 'l' => 'ˡ', 'm' => 'ᵐ', 'n' => 'ⁿ', 'o' => 'ᵒ',
        'p' => 'ᵖ', 'r' => 'ʳ', 's' => 'ˢ', 't' => 'ᵗ', 'u' => 'ᵘ',
        'v' => 'ᵛ', 'w' => 'ᵂ', 'x' => 'ˣ', 'y' => 'ʸ', 'z' => 'ᶻ',
        '+' => '⁺', '-' => '⁻', '=' => '⁼', '(' => '⁽', ')' => '⁾',
        _ => return None,
    })
}

fn subscript(c: char) -> Option<char> {
    Some(match c {
        '0' => '₀', '1' => '₁', '2' => '₂', '3' => '₃', '4' => '₄',
        '5' => '₅', '6' => '₆', '7' => '₇', '8' => '₈', '9' => '₉',
        'a' => 'ₐ', 'e' => 'ₑ', 'h' => 'ₕ', 'i' => 'ᵢ', 'j' => 'ⱼ',
        'k' => 'ₖ', 'l' => 'ₗ', 'm' => 'ₘ', 'n' => 'ₙ', 'o' => 'ₒ',
        'p' => 'ₚ', 'r' => 'ᵣ', 's' => 'ₛ', 't' => 'ₜ', 'u' => 'ᵤ',
        'v' => 'ᵥ', 'x' => 'ₓ', '+' => '₊', '-' => '₋', '=' => '₌',
        '(' => '₍', ')' => '₎',
        _ => return None,
    })
}

/// The formula's visual form: Unicode symbols, raised and lowered digits.
pub fn to_unicode(source: &str) -> String {
    let mut out = String::with_capacity(source.len());
    let chars: Vec<char> = source.chars().collect();
    let mut i = 0usize;
    while i < chars.len() {
        let c = chars[i];
        if c == '\\' {
            // A command: `\alpha`, `\sqrt{...}`, `\frac{a}{b}`.
            i += 1;
            if i >= chars.len() {
                out.push('\\');
                break;
            }
            // `\ ` is a space, `\{` and `\}` are braces.
            if chars[i].is_whitespace() {
                out.push(' ');
                i += 1;
                continue;
            }
            if chars[i] == '{' || chars[i] == '}' || chars[i] == '%' || chars[i] == '&' {
                out.push(chars[i]);
                i += 1;
                continue;
            }
            let start = i;
            while i < chars.len() && chars[i].is_ascii_alphabetic() {
                i += 1;
            }
            if i == start {
                // `\!`, `\,`, `\;` — spacing commands, and `\\` a newline.
                out.push(' ');
                i += 1;
                continue;
            }
            let name: String = chars[start..i].iter().collect();
            match name.as_str() {
                "frac" => {
                    // \frac{a}{b} → (a)/(b): the grouping survives plain text.
                    let a = take_group(&chars, &mut i);
                    let b = take_group(&chars, &mut i);
                    out.push('(');
                    out.push_str(&to_unicode(&a));
                    out.push_str(")/(");
                    out.push_str(&to_unicode(&b));
                    out.push(')');
                }
                "sqrt" => {
                    let inner = take_optional_group(&chars, &mut i);
                    out.push('√');
                    if !inner.is_empty() {
                        out.push('(');
                        out.push_str(&to_unicode(&inner));
                        out.push(')');
                    }
                }
                "frac12" | "frac14" | "frac34" => {
                    out.push_str(match name.as_str() {
                        "frac12" => "½", "frac14" => "¼", _ => "¾",
                    });
                }
                "left" | "right" | "big" | "Big" | "bigg" | "Bigg"
                | "middle" | "displaystyle" | "limits" | "nolimits" => {
                    // Sizing and delimiter commands: the shape is dropped,
                    // the delimiter itself is kept by the next character.
                }
                _ => {
                    let replacement = SYMBOLS
                        .iter()
                        .find(|(n, _)| *n == name)
                        .map(|(_, s)| *s);
                    match replacement {
                        Some(sym) => out.push_str(sym),
                        None => {
                            // Unknown: keep the name, so nothing is lost.
                            out.push('\\');
                            out.push_str(&name);
                        }
                    }
                }
            }
            continue;
        }
        if c == '^' || c == '_' {
            // A superscript or subscript: the next group, raised or lowered.
            i += 1;
            let group = take_optional_group(&chars, &mut i);
            let target = if c == '^' { superscript } else { subscript };
            let mut any = false;
            for g in group.chars() {
                match target(g) {
                    Some(s) => {
                        out.push(s);
                        any = true;
                    }
                    None => {
                        if any {
                            out.push(' ');
                        }
                        out.push(g);
                        any = false;
                    }
                }
            }
            continue;
        }
        if c == '{' || c == '}' {
            // A grouping that is not part of a command: the braces are
            // grouping, not content.
            i += 1;
            continue;
        }
        if c == '&' {
            // An alignment marker: a space between the columns.
            out.push(' ');
            i += 1;
            continue;
        }
        out.push(c);
        i += 1;
    }
    out.trim().to_string()
}

/// Does this span read as money rather than as a formula?
///
/// A note's currency is not a formula: `$5`, `$1,200.50`, `€20`, `£3.99`.
/// The tell is that the span is a number with a currency marker and
/// nothing else — no letters, no operators, no variables.
pub fn reads_as_money(source: &str) -> bool {
    let text = source.trim();
    if text.is_empty() {
        return false;
    }
    // Strip a leading currency symbol and a trailing percent sign.
    let body = text.trim_start_matches(['$', '€', '£', '¥', '₹', '₽', '¢']);
    let body = body.trim_end_matches('%');
    if body.is_empty() {
        return false;
    }
    // Digits, thousands separators and at most one decimal point — and
    // nothing else. A space or a letter means it is not an amount.
    let mut seen_digit = false;
    let mut seen_point = false;
    for c in body.chars() {
        match c {
            '0'..='9' => seen_digit = true,
            ',' | ' ' | '_' => {}
            '.' if !seen_point => seen_point = true,
            _ => return false,
        }
    }
    seen_digit
}

/// The formula's spoken form: words, not symbols.
///
/// A voice reading "x² + √(y)" aloud needs names: "x squared plus the
/// square root of y".
pub fn to_speech(source: &str) -> String {
    let mut out = String::with_capacity(source.len());
    let chars: Vec<char> = source.chars().collect();
    let mut i = 0usize;
    while i < chars.len() {
        let c = chars[i];
        if c == '\\' {
            i += 1;
            if i >= chars.len() {
                out.push_str("backslash");
                break;
            }
            let start = i;
            while i < chars.len() && chars[i].is_ascii_alphabetic() {
                i += 1;
            }
            if i == start {
                i += 1;
                continue;
            }
            let name: String = chars[start..i].iter().collect();
            match name.as_str() {
                "frac" => {
                    let a = take_group(&chars, &mut i);
                    let b = take_group(&chars, &mut i);
                    out.push_str(&to_speech(&a));
                    out.push_str(" over ");
                    out.push_str(&to_speech(&b));
                }
                "sqrt" => {
                    let inner = take_optional_group(&chars, &mut i);
                    out.push_str("the square root of ");
                    out.push_str(&to_speech(&inner));
                }
                "sum" => out.push_str("the sum of "),
                "prod" => out.push_str("the product of "),
                "int" => out.push_str("the integral of "),
                "infty" => out.push_str("infinity"),
                "times" => out.push_str(" times "),
                "div" => out.push_str(" divided by "),
                "pm" => out.push_str(" plus or minus "),
                "neq" => out.push_str(" is not equal to "),
                "leq" | "le" => out.push_str(" is at most "),
                "geq" | "ge" => out.push_str(" is at least "),
                "approx" => out.push_str(" is approximately "),
                "rightarrow" | "to" => out.push_str(" maps to "),
                "leftarrow" => out.push_str(" maps from "),
                "alpha" => out.push_str("alpha"),
                "beta" => out.push_str("beta"),
                "gamma" => out.push_str("gamma"),
                "delta" => out.push_str("delta"),
                "theta" => out.push_str("theta"),
                "lambda" => out.push_str("lambda"),
                "mu" => out.push_str("mu"),
                "pi" => out.push_str("pi"),
                "sigma" => out.push_str("sigma"),
                "phi" => out.push_str("phi"),
                "omega" => out.push_str("omega"),
                "text" | "mathrm" | "mathbf" | "mathit" | "mathbb"
                | "mathcal" | "mathsf" | "mathtt" | "operatorname" => {
                    let inner = take_optional_group(&chars, &mut i);
                    out.push_str(&to_speech(&inner));
                }
                "left" | "right" | "big" | "Big" | "bigg" | "Bigg" => {}
                _ => {
                    // An unknown command: say its name, letters separated.
                    out.push_str(&name);
                }
            }
            continue;
        }
        if c == '^' {
            i += 1;
            let group = take_optional_group(&chars, &mut i);
            out.push(' ');
            out.push_str(&spell_power(&group, "to the power of "));
            continue;
        }
        if c == '_' {
            i += 1;
            let group = take_optional_group(&chars, &mut i);
            out.push_str(" subscript ");
            out.push_str(&group);
            continue;
        }
        if c == '{' || c == '}' {
            i += 1;
            continue;
        }
        if c == '^' {
            i += 1;
            continue;
        }
        match c {
            '=' => out.push_str(" equals "),
            '<' => out.push_str(" is less than "),
            '>' => out.push_str(" is greater than "),
            '+' => out.push_str(" plus "),
            '-' => out.push_str(" minus "),
            '*' => out.push_str(" times "),
            '/' => out.push_str(" divided by "),
            '&' => out.push(' '),
            _ => out.push(c),
        }
        i += 1;
    }
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// "2" → "squared", "3" → "cubed", "n" → "to the power of n".
fn spell_power(group: &str, prefix: &str) -> String {
    match group {
        "2" => "squared".to_string(),
        "3" => "cubed".to_string(),
        "1" => String::new(),
        other => format!("{prefix}{other}"),
    }
}

/// A `{...}` group, taken from the source: the braces and their content.
fn take_group(chars: &[char], i: &mut usize) -> String {
    while *i < chars.len() && chars[*i].is_whitespace() {
        *i += 1;
    }
    if *i >= chars.len() || chars[*i] != '{' {
        return String::new();
    }
    *i += 1;
    let start = *i;
    let mut depth = 1usize;
    while *i < chars.len() {
        match chars[*i] {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    break;
                }
            }
            _ => {}
        }
        *i += 1;
    }
    let text: String = chars[start..*i].iter().collect();
    if *i < chars.len() {
        *i += 1; // the closing brace
    }
    text
}

/// A group if there is one, a single token otherwise: `\sqrt{x}` and
/// `\sqrt x` are both the square root of x.
fn take_optional_group(chars: &[char], i: &mut usize) -> String {
    while *i < chars.len() && chars[*i].is_whitespace() {
        *i += 1;
    }
    if *i < chars.len() && chars[*i] == '{' {
        return take_group(chars, i);
    }
    if *i >= chars.len() {
        return String::new();
    }
    // One token: a command, or a single character.
    if chars[*i] == '\\' {
        let start = *i;
        *i += 1;
        while *i < chars.len() && chars[*i].is_ascii_alphabetic() {
            *i += 1;
        }
        if *i == start + 1 {
            *i += 1;
        }
        return chars[start..*i].iter().collect();
    }
    let c = chars[*i];
    *i += 1;
    c.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn greek_and_operators_become_characters() {
        assert_eq!(to_unicode(r"\alpha + \beta"), "α + β");
        assert_eq!(to_unicode(r"x \times y"), "x × y");
        assert_eq!(to_unicode(r"a \leq b"), "a ≤ b");
        assert_eq!(to_unicode(r"\infty"), "∞");
    }

    #[test]
    fn exponents_become_raised_digits() {
        assert_eq!(to_unicode("x^2"), "x²");
        assert_eq!(to_unicode("x^{10}"), "x¹⁰");
        assert_eq!(to_unicode("H_2O"), "H₂O");
    }

    #[test]
    fn roots_and_fractions_keep_their_meaning() {
        assert_eq!(to_unicode(r"\sqrt{x}"), "√(x)");
        assert_eq!(to_unicode(r"\frac{a}{b}"), "(a)/(b)");
    }

    #[test]
    fn an_unknown_command_is_kept_not_dropped() {
        assert_eq!(to_unicode(r"\foo{x}"), "\\foox");
    }

    #[test]
    fn the_spoken_form_says_the_words() {
        assert_eq!(to_speech("x^2"), "x squared");
        assert_eq!(to_speech(r"\frac{a}{b}"), "a over b");
        assert_eq!(to_speech(r"\sqrt{y}"), "the square root of y");
        assert_eq!(to_speech(r"\alpha"), "alpha");
    }
}
