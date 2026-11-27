use crate::{ForegroundSource as Sdk, ForegroundSources, ForegroundState};
use rmux_proto::{ForegroundFieldSource as Wire, ForegroundSourcesDto, ForegroundStateDto};

// Literal expectations are independent of the production DTO conversions.
pub(super) fn cases() -> Vec<(Option<ForegroundStateDto>, Option<ForegroundState>)> {
    let mut cases = vec![
        (None, None),
        (
            Some(ForegroundStateDto::default()),
            Some(ForegroundState::default()),
        ),
        (
            Some(ForegroundStateDto {
                pid: None,
                command: Some("".into()),
                cwd: None,
                exe: Some(" ./bin/λ ".into()),
                sources: ForegroundSourcesDto {
                    pid: Some(Wire::Environment),
                    command: None,
                    cwd: Some(Wire::Osc7),
                    exe: None,
                },
            }),
            Some(ForegroundState {
                pid: None,
                command: Some("".into()),
                cwd: None,
                exe: Some(" ./bin/λ ".into()),
                sources: ForegroundSources {
                    pid: Some(Sdk::Environment),
                    command: None,
                    cwd: Some(Sdk::Osc7),
                    exe: None,
                },
            }),
        ),
        (
            Some(ForegroundStateDto {
                pid: Some(0),
                command: None,
                cwd: Some("".into()),
                exe: None,
                sources: ForegroundSourcesDto {
                    pid: None,
                    command: Some(Wire::RuntimeName),
                    cwd: None,
                    exe: Some(Wire::Profile),
                },
            }),
            Some(ForegroundState {
                pid: Some(0),
                command: None,
                cwd: Some("".into()),
                exe: None,
                sources: ForegroundSources {
                    pid: None,
                    command: Some(Sdk::RuntimeName),
                    cwd: None,
                    exe: Some(Sdk::Profile),
                },
            }),
        ),
    ];
    // Rotate distinct labels to detect field swaps as well as variant swaps.
    let sources = [
        (
            [Wire::Process, Wire::RootProcess, Wire::Osc7, Wire::Profile],
            [Sdk::Process, Sdk::RootProcess, Sdk::Osc7, Sdk::Profile],
        ),
        (
            [
                Wire::RootProcess,
                Wire::Osc7,
                Wire::Profile,
                Wire::Environment,
            ],
            [Sdk::RootProcess, Sdk::Osc7, Sdk::Profile, Sdk::Environment],
        ),
        (
            [
                Wire::Osc7,
                Wire::Profile,
                Wire::Environment,
                Wire::RuntimeName,
            ],
            [Sdk::Osc7, Sdk::Profile, Sdk::Environment, Sdk::RuntimeName],
        ),
        (
            [
                Wire::Profile,
                Wire::Environment,
                Wire::RuntimeName,
                Wire::Process,
            ],
            [
                Sdk::Profile,
                Sdk::Environment,
                Sdk::RuntimeName,
                Sdk::Process,
            ],
        ),
        (
            [
                Wire::Environment,
                Wire::RuntimeName,
                Wire::Process,
                Wire::RootProcess,
            ],
            [
                Sdk::Environment,
                Sdk::RuntimeName,
                Sdk::Process,
                Sdk::RootProcess,
            ],
        ),
        (
            [
                Wire::RuntimeName,
                Wire::Process,
                Wire::RootProcess,
                Wire::Osc7,
            ],
            [Sdk::RuntimeName, Sdk::Process, Sdk::RootProcess, Sdk::Osc7],
        ),
    ];
    for (wire, expected) in sources {
        cases.push((
            Some(ForegroundStateDto {
                pid: Some(4242),
                command: Some("Cmd λ".into()),
                cwd: Some(" /work/../λ ".into()),
                exe: Some("C:\\Tools\\App.EXE".into()),
                sources: ForegroundSourcesDto {
                    pid: Some(wire[0]),
                    command: Some(wire[1]),
                    cwd: Some(wire[2]),
                    exe: Some(wire[3]),
                },
            }),
            Some(ForegroundState {
                pid: Some(4242),
                command: Some("Cmd λ".into()),
                cwd: Some(" /work/../λ ".into()),
                exe: Some("C:\\Tools\\App.EXE".into()),
                sources: ForegroundSources {
                    pid: Some(expected[0]),
                    command: Some(expected[1]),
                    cwd: Some(expected[2]),
                    exe: Some(expected[3]),
                },
            }),
        ));
    }
    cases
}
