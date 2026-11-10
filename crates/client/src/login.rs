//! The login screen: one password box in front of an otherwise locked desktop.
//!
//! Shown only when the server says a password is configured and this browser has no
//! session (see [`AuthState`]). It deliberately says little — no username, since there
//! is one account, and no hint about whether a wrong password was close.

use leptos::html;
use leptos::prelude::*;

use crate::state::Desktop;

/// Full-screen sign-in card. Rendered by `App` in place of the shell while locked.
#[component]
pub fn LoginScreen() -> impl IntoView {
    let desktop = expect_context::<Desktop>();
    let input = NodeRef::<html::Input>::new();

    let submit = move || {
        let Some(field) = input.get() else {
            return;
        };
        let password = field.value();
        if password.is_empty() {
            desktop
                .login_error
                .set(Some("Enter the password to continue.".to_owned()));
            return;
        }
        // Cleared straight away, so the value is not sitting in the DOM while the
        // request is in flight.
        field.set_value("");
        desktop.sign_in(password);
    };

    let on_enter = move |event: leptos::ev::KeyboardEvent| {
        if event.key() == "Enter" {
            submit();
        }
    };

    // Focus the box once it exists. Reading the `NodeRef` inside the effect is what
    // makes this run after the node is attached rather than on the first render.
    Effect::new(move |_| {
        if let Some(field) = input.get() {
            let _ = field.focus();
        }
    });

    view! {
        <div class="login">
            <form class="login-card" on:submit=move |event| { event.prevent_default(); submit(); }>
                <div class="login-mark">"🔒"</div>
                <h1>"Web Desktop"</h1>
                <p class="muted">"This desktop is locked. Enter the password to continue."</p>

                <label class="login-field">
                    <span>"Password"</span>
                    <input
                        node_ref=input
                        type="password"
                        autocomplete="current-password"
                        placeholder="••••••••"
                        disabled=move || desktop.login_busy.get()
                        on:keydown=on_enter
                    />
                </label>

                <Show when=move || desktop.login_error.get().is_some()>
                    <p class="login-error" role="alert">
                        {move || desktop.login_error.get().unwrap_or_default()}
                    </p>
                </Show>

                <button
                    class="login-btn"
                    type="submit"
                    disabled=move || desktop.login_busy.get()
                >
                    {move || if desktop.login_busy.get() { "Checking…" } else { "Unlock" }}
                </button>

                <p class="login-hint muted">
                    "The password is checked by the server; it is never stored in this browser."
                </p>
            </form>
        </div>
    }
}
