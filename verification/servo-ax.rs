// Verus proof of the SHIPPED code in our Servo fork (the rule): what a form field shows in the accessibility
// tree, and which actions an element advertises and accepts. The path is the fork this tree carries;
// scripts/verify.sh can point it at another checkout (LUCINANET_SERVO).
#[path = "../third_party/servo/components/shared/layout/accessibility_rules.rs"]
pub mod rules;

fn main() {}
