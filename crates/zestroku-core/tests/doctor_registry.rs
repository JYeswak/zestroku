#![forbid(unsafe_code)]

use zestroku_core::doctor::{
    CheckDefinition, CheckId, CheckOutcome, CheckRegistry, CheckStatus, DoctorSession,
};

const DEPENDENT_CHECKS: &[CheckDefinition] = &[
    CheckDefinition {
        id: CheckId::DataDir,
        prerequisites: &[],
    },
    CheckDefinition {
        id: CheckId::Store,
        prerequisites: &[CheckId::DataDir],
    },
    CheckDefinition {
        id: CheckId::Migrations,
        prerequisites: &[CheckId::Store],
    },
    CheckDefinition {
        id: CheckId::SsdpResponders,
        prerequisites: &[],
    },
];

struct FixtureRegistry(&'static [CheckDefinition]);

impl CheckRegistry for FixtureRegistry {
    fn checks(&self) -> &'static [CheckDefinition] {
        self.0
    }
}

fn report_with(status: CheckStatus) -> zestroku_core::doctor::DoctorReport {
    static ONE_CHECK: &[CheckDefinition] = &[CheckDefinition {
        id: CheckId::DataDir,
        prerequisites: &[],
    }];
    let mut session = DoctorSession::new(&FixtureRegistry(ONE_CHECK)).expect("valid registry");
    assert_eq!(session.next_check().expect("next check"), Some(CheckId::DataDir));
    session
        .record(CheckOutcome {
            status,
            detail: "fixture outcome".into(),
            evidence: Vec::new(),
            remedy: None,
            code: None,
        })
        .expect("record one check");
    assert_eq!(session.next_check().expect("finished checks"), None);
    session.finish().expect("complete report")
}

#[test]
fn failed_prerequisites_skip_dependents_and_exit_codes_aggregate() {
    let mut session = DoctorSession::new(&FixtureRegistry(DEPENDENT_CHECKS))
        .expect("valid prerequisite order");
    assert_eq!(session.next_check().expect("data dir"), Some(CheckId::DataDir));
    session
        .record(CheckOutcome {
            status: CheckStatus::Fail,
            detail: "data directory unreadable".into(),
            evidence: vec!["directory metadata failed".into()],
            remedy: Some("restore directory access".into()),
            code: Some("DATA_DIR_UNREADABLE".into()),
        })
        .expect("record failed prerequisite");

    assert_eq!(
        session.next_check().expect("skip failed dependents"),
        Some(CheckId::SsdpResponders),
        "a failed prerequisite skips dependents without suppressing independent checks"
    );
    session
        .record(CheckOutcome {
            status: CheckStatus::Pass,
            detail: "loopback responder observed".into(),
            evidence: vec!["one responder".into()],
            remedy: None,
            code: None,
        })
        .expect("record independent check");
    assert_eq!(session.next_check().expect("all checks done"), None);

    let report = session.finish().expect("complete report");
    assert_eq!(report.exit_code, 7);
    assert_eq!(report.checks.len(), 4);
    assert_eq!(report.checks[0].status, CheckStatus::Fail);
    assert_eq!(report.checks[1].status, CheckStatus::Skip);
    assert_eq!(report.checks[2].status, CheckStatus::Skip);
    assert_eq!(report.checks[3].status, CheckStatus::Pass);
    assert_eq!(report.checks[1].prerequisites, vec![CheckId::DataDir]);
    assert_eq!(report.checks[2].prerequisites, vec![CheckId::Store]);

    assert_eq!(report_with(CheckStatus::Pass).exit_code, 0);
    assert_eq!(report_with(CheckStatus::Warn).exit_code, 6);
    assert_eq!(report_with(CheckStatus::Fail).exit_code, 7);
}
