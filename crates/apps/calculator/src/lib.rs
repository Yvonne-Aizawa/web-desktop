//! A four-function calculator, built as a served app.
//!
//! Everything the shell provides arrives through [`Context`]; everything else is
//! local. Building it with `cargo xtask build-app crates/apps/calculator` produces
//! `apps/calculator/0.1.0/{manifest.json,app.js,app_bg.wasm}`, which the server
//! advertises in the catalog like any other app.
//!
//! Note the `<style>` block: a served app cannot rely on the shell's stylesheet, so
//! it brings its own. The palette still comes from `ctx.theme()`.

use std::rc::Rc;

use app_sdk::prelude::*;

/// One press on the keypad.
#[derive(Clone, Copy, PartialEq)]
enum Key {
    Digit(u8),
    Dot,
    Op(Op),
    Equals,
    Clear,
    Sign,
}

#[derive(Clone, Copy, PartialEq)]
enum Op {
    Add,
    Sub,
    Mul,
    Div,
}

impl Op {
    fn symbol(self) -> &'static str {
        match self {
            Self::Add => "+",
            Self::Sub => "-",
            Self::Mul => "*",
            Self::Div => "/",
        }
    }

    fn apply(self, left: f64, right: f64) -> f64 {
        match self {
            Self::Add => left + right,
            Self::Sub => left - right,
            Self::Mul => left * right,
            Self::Div => left / right,
        }
    }
}

/// Trim the float noise a calculator always produces: 0.1 + 0.2 should read 0.3.
fn format(value: f64) -> String {
    if !value.is_finite() {
        return "not a number".to_owned();
    }
    let rounded = format!("{value:.10}");
    let trimmed = rounded.trim_end_matches('0').trim_end_matches('.');
    if trimmed.is_empty() || trimmed == "-" {
        "0".to_owned()
    } else {
        trimmed.to_owned()
    }
}

const STYLES: &str = "
.calculator { display: flex; flex-direction: column; gap: 10px; padding: 12px;
              height: 100%; box-sizing: border-box; font-family: inherit; }
.calc-display { display: block; padding: 12px 14px; border-radius: 10px; text-align: right;
                font-family: ui-monospace, monospace; font-size: 26px; letter-spacing: 0.02em;
                background: rgba(0,0,0,.35); overflow: hidden; text-overflow: ellipsis;
                white-space: nowrap; }
.calc-keys { flex: 1; display: grid; grid-template-columns: repeat(4, 1fr); gap: 8px; }
.calc-key { border: 1px solid; border-radius: 10px; background: rgba(255,255,255,.05);
            font-size: 17px; font-family: inherit; cursor: pointer; color: inherit;
            transition: background 90ms ease, transform 60ms ease; }
.calc-key:hover { background: rgba(255,255,255,.12); }
.calc-key:active { transform: scale(0.97); }
.calc-key.zero { grid-column: span 3; }
.calc-foot { margin: 0; font-size: 10.5px; text-align: center; opacity: .6;
             font-family: ui-monospace, monospace; }
";

app_sdk::app! {
    fn calculator(ctx: Context) -> impl IntoView {
        let theme = ctx.theme();
        let size = ctx.size();

        let entry = RwSignal::new("0".to_owned());
        let accumulator = RwSignal::new(None::<f64>);
        let pending = RwSignal::new(None::<Op>);
        let fresh = RwSignal::new(true);

        // `Rc` so the keypad builder can hand each key its own handle: a plain
        // closure capturing `ctx` is neither `Copy` nor reusable across keys.
        let press: Rc<dyn Fn(Key)> = {
            let ctx = ctx.clone();
            Rc::new(move |key: Key| match key {
                Key::Digit(d) => {
                    if fresh.get_untracked() {
                        entry.set(d.to_string());
                        fresh.set(false);
                    } else {
                        let mut current = entry.get_untracked();
                        if current == "0" {
                            current.clear();
                        }
                        current.push(char::from(b'0' + d));
                        entry.set(current);
                    }
                }
                Key::Dot => {
                    if fresh.get_untracked() {
                        entry.set("0.".to_owned());
                        fresh.set(false);
                    } else if !entry.get_untracked().contains('.') {
                        entry.update(|text| text.push('.'));
                    }
                }
                Key::Sign => {
                    if let Ok(value) = entry.get_untracked().parse::<f64>() {
                        entry.set(format(-value));
                    }
                }
                Key::Clear => {
                    entry.set("0".to_owned());
                    accumulator.set(None);
                    pending.set(None);
                    fresh.set(true);
                }
                Key::Op(op) => {
                    // Fold the previous operation first, so 2 + 3 + 4 works.
                    let current = entry.get_untracked().parse::<f64>().unwrap_or(0.0);
                    let value = match (accumulator.get_untracked(), pending.get_untracked()) {
                        (Some(left), Some(previous)) => previous.apply(left, current),
                        _ => current,
                    };
                    accumulator.set(Some(value));
                    pending.set(Some(op));
                    entry.set(format(value));
                    fresh.set(true);
                }
                Key::Equals => {
                    let current = entry.get_untracked().parse::<f64>().unwrap_or(0.0);
                    if let (Some(left), Some(op)) =
                        (accumulator.get_untracked(), pending.get_untracked())
                    {
                        let result = op.apply(left, current);
                        ctx.log(&format!("{left} {} {current} = {result}", op.symbol()));
                        entry.set(format(result));
                        accumulator.set(None);
                        pending.set(None);
                        fresh.set(true);
                    }
                }
            })
        };

        let keypad = {
            let press = Rc::clone(&press);
            let accent = theme.accent.clone();
            let stroke = theme.stroke.clone();
            move |key: Key, label: &'static str, class: &'static str| {
                let press = Rc::clone(&press);
                let accent = accent.clone();
                let stroke = stroke.clone();
                let style = move || {
                    if matches!(key, Key::Equals) {
                        format!("background:{accent};color:#0b0d12;border-color:{accent};")
                    } else {
                        format!("border-color:{stroke};")
                    }
                };
                view! {
                    <button class=format!("calc-key {class}") style=style on:click=move |_| press(key)>
                        {label}
                    </button>
                }
            }
        };

        let ctx_foot = ctx.clone();
        view! {
            <style>{STYLES}</style>
            <div class="calculator">
                <output class="calc-display">{move || entry.get()}</output>
                <div class="calc-keys">
                    {keypad(Key::Clear, "C", "")}
                    {keypad(Key::Sign, "±", "")}
                    {keypad(Key::Op(Op::Div), "÷", "")}
                    {keypad(Key::Op(Op::Mul), "×", "")}

                    {keypad(Key::Digit(7), "7", "")}
                    {keypad(Key::Digit(8), "8", "")}
                    {keypad(Key::Digit(9), "9", "")}
                    {keypad(Key::Op(Op::Sub), "−", "")}

                    {keypad(Key::Digit(4), "4", "")}
                    {keypad(Key::Digit(5), "5", "")}
                    {keypad(Key::Digit(6), "6", "")}
                    {keypad(Key::Op(Op::Add), "+", "")}

                    {keypad(Key::Digit(1), "1", "")}
                    {keypad(Key::Digit(2), "2", "")}
                    {keypad(Key::Digit(3), "3", "")}
                    {keypad(Key::Equals, "=", "")}

                    {keypad(Key::Digit(0), "0", "zero")}
                    {keypad(Key::Dot, ".", "")}
                </div>
                <p class="calc-foot">
                    {move || format!(
                        "{}×{} · window #{} · served app",
                        size.width as i32,
                        size.height as i32,
                        ctx_foot.window_id(),
                    )}
                </p>
            </div>
        }
    }
}
