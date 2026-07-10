use rmux_core::{HookBindingView, HookGlobalRoot, HookStore};
use rmux_proto::{HookName, OptionScopeSelector, ShowOptionsRequest, WindowTarget};

use super::super::HandlerState;

#[derive(Debug, Clone, Copy)]
struct HookFilter {
    hook: HookName,
    index: Option<u32>,
}

pub(super) fn render_named_hook(
    state: &HandlerState,
    request: &ShowOptionsRequest,
) -> Option<Vec<String>> {
    let filter = parse_hook_filter(request.name.as_deref()?)?;
    Some(render_hooks(state, request, Some(filter)))
}

pub(super) fn render_included_hooks(
    state: &HandlerState,
    request: &ShowOptionsRequest,
) -> Vec<String> {
    render_hooks(state, request, None)
}

fn render_hooks(
    state: &HandlerState,
    request: &ShowOptionsRequest,
    filter: Option<HookFilter>,
) -> Vec<String> {
    match &request.scope {
        OptionScopeSelector::ServerGlobal => Vec::new(),
        OptionScopeSelector::SessionGlobal => render_global(
            &state.hooks,
            HookGlobalRoot::Session,
            filter,
            request.value_only,
        ),
        OptionScopeSelector::WindowGlobal => render_global(
            &state.hooks,
            HookGlobalRoot::Window,
            filter,
            request.value_only,
        ),
        OptionScopeSelector::Session(session_name) => render_local(
            &state.hooks,
            HookGlobalRoot::Session,
            state
                .hooks
                .session_bindings_view(session_name, hook(filter)),
            Vec::new(),
            filter,
            request,
        ),
        OptionScopeSelector::Window(target) => render_local(
            &state.hooks,
            HookGlobalRoot::Window,
            state.hooks.window_bindings_view(target, hook(filter)),
            Vec::new(),
            filter,
            request,
        ),
        OptionScopeSelector::Pane(target) => {
            let window =
                WindowTarget::with_window(target.session_name().clone(), target.window_index());
            render_local(
                &state.hooks,
                HookGlobalRoot::Window,
                state.hooks.pane_bindings_view(target, hook(filter)),
                state.hooks.window_bindings_view(&window, hook(filter)),
                filter,
                request,
            )
        }
    }
}

fn render_global(
    store: &HookStore,
    root: HookGlobalRoot,
    filter: Option<HookFilter>,
    value_only: bool,
) -> Vec<String> {
    let explicit = store.global_bindings_view(root, hook(filter));
    HookStore::shipped_global_hooks(root, hook(filter))
        .into_iter()
        .flat_map(|hook_name| {
            let bindings = matching_bindings(&explicit, hook_name, filter.and_then(|f| f.index));
            if bindings.is_empty() {
                render_empty_hook(hook_name, filter.and_then(|f| f.index), false, value_only)
            } else {
                render_bindings(bindings, false, value_only)
            }
        })
        .collect()
}

fn render_local(
    store: &HookStore,
    root: HookGlobalRoot,
    explicit: Vec<HookBindingView>,
    intermediate: Vec<HookBindingView>,
    filter: Option<HookFilter>,
    request: &ShowOptionsRequest,
) -> Vec<String> {
    if !request.include_inherited {
        return render_bindings(
            filter_bindings(&explicit, filter),
            false,
            request.value_only,
        );
    }

    let global = store.global_bindings_view(root, hook(filter));
    HookStore::shipped_global_hooks(root, hook(filter))
        .into_iter()
        .flat_map(|hook_name| {
            let index = filter.and_then(|f| f.index);
            let local = matching_bindings(&explicit, hook_name, index);
            if !local.is_empty() {
                return render_bindings(local, false, request.value_only);
            }
            let parent = matching_bindings(&intermediate, hook_name, index);
            if !parent.is_empty() {
                return render_bindings(parent, true, request.value_only);
            }
            let inherited = matching_bindings(&global, hook_name, index);
            if inherited.is_empty() {
                render_empty_hook(hook_name, index, true, request.value_only)
            } else {
                render_bindings(inherited, true, request.value_only)
            }
        })
        .collect()
}

fn render_bindings(
    bindings: Vec<&HookBindingView>,
    inherited: bool,
    value_only: bool,
) -> Vec<String> {
    bindings
        .into_iter()
        .map(|binding| {
            if value_only {
                binding.command().to_owned()
            } else {
                let marker = if inherited { "*" } else { "" };
                format!(
                    "{}[{}]{marker} {}",
                    binding.hook(),
                    binding.index(),
                    binding.command()
                )
            }
        })
        .collect()
}

fn render_empty_hook(
    hook: HookName,
    index: Option<u32>,
    inherited: bool,
    value_only: bool,
) -> Vec<String> {
    if value_only {
        return Vec::new();
    }
    let marker = if inherited { "*" } else { "" };
    vec![match index {
        Some(index) => format!("{hook}[{index}]{marker} "),
        None => format!("{hook}{marker}"),
    }]
}

fn matching_bindings(
    bindings: &[HookBindingView],
    hook: HookName,
    index: Option<u32>,
) -> Vec<&HookBindingView> {
    bindings
        .iter()
        .filter(|binding| binding.hook() == hook)
        .filter(|binding| index.map(|index| binding.index() == index).unwrap_or(true))
        .collect()
}

fn filter_bindings(
    bindings: &[HookBindingView],
    filter: Option<HookFilter>,
) -> Vec<&HookBindingView> {
    bindings
        .iter()
        .filter(|binding| {
            filter
                .map(|filter| {
                    binding.hook() == filter.hook
                        && filter
                            .index
                            .map(|index| binding.index() == index)
                            .unwrap_or(true)
                })
                .unwrap_or(true)
        })
        .collect()
}

fn hook(filter: Option<HookFilter>) -> Option<HookName> {
    filter.map(|filter| filter.hook)
}

fn parse_hook_filter(value: &str) -> Option<HookFilter> {
    let (name, index) = match value.rsplit_once('[') {
        Some((name, index)) => (name, Some(index.strip_suffix(']')?.parse::<u32>().ok()?)),
        None => (value, None),
    };
    Some(HookFilter {
        hook: HookName::from_str(name)?,
        index,
    })
}
