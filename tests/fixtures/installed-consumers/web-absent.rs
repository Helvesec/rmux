// This import must fail whenever the installed SDK has Web disabled.
use rmux_sdk::WebShareHandle;
fn main() { let _: Option<WebShareHandle> = None; }
