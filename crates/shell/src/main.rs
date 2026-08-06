//! The browser shell: window, chrome, tabs, input, and navigation.
//!
//! Stub: M0 establishes the workspace only. M1 replaces this with a window that
//! renders a fetched document; M3 builds the chrome around it. See PLAN.md.

fn main() {
    // Deliberately not a window yet. M0's job is to make M1 startable, and
    // claiming more than that in the binary would make the budget numbers
    // (tests/budgets) measure something that does not exist.
    println!(
        "2kbrowser {} — M0 scaffold, no engine yet.",
        env!("CARGO_PKG_VERSION")
    );
    println!("See PLAN.md for what lands in M1.");
}
