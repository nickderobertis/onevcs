# Red, then green

Every journey this branch adds, observed failing for the behaviour it is
about before it passed. Regenerate with `just red-green`, which re-applies
each mutation under `scripts/red-green/`, records the assertion the test
failed on, reverts it, and then runs the same tests green.

Patches: 19. Tests observed red and then green: 31.

### `01-the-verb-has-no-implementation`

publish-branch answers NotImplemented, which is the state before this branch: the verb parses and nothing is behind it.

- RED `a_complete_branch_of_a_local_identity_lands_on_its_base` — Unexpected failure.
- RED `a_complete_branch_of_a_team_identity_opens_the_change_request_its_rules_require` — Unexpected failure.
- RED `a_complete_branch_of_a_remote_identity_is_landed_by_the_host` — Unexpected failure.
- RED `a_branch_the_host_holds_reports_the_merge_it_queued` — Unexpected failure.
- RED `a_hosted_origin_this_build_does_not_speak_for_answers_the_seam_it_has_no_body_for` — Unexpected stderr, failed var.contains(RemoteHost for a host other than github.com is not implemented yet)
- RED `an_identity_with_no_rules_file_publishes_under_the_built_in_default` — Unexpected failure.
- RED `publishing_a_branch_refuses_interrupted_work_and_names_the_verb_that_lands_it` — Unexpected return code, failed var == 2
- RED `a_branch_no_checkout_has_is_refused_by_the_command_that_lists_the_ones_that_do` — Unexpected return code, failed var == 2
- RED `an_explicit_title_is_the_subject_a_branch_publishes_under` — Unexpected failure.
- RED `a_branch_with_no_usable_subject_is_refused_until_a_title_names_the_change` — Unexpected return code, failed var == 2
- RED `a_per_run_policy_narrows_the_rules_resolved_one_and_never_widens_it` — Unexpected return code, failed var == 2
- RED `a_gate_that_rejects_a_branch_keeps_it_where_it_was_found` — Unexpected return code, failed var == 1
- RED `a_change_base_that_conflicts_is_refused_once_and_lands_after_it_is_resolved` — Unexpected return code, failed var == 3
- RED `a_marker_under_an_unreadable_prefix_is_never_published_as_a_finished_branch` — Unexpected return code, failed var == 2
- RED `a_repository_path_that_is_not_text_is_refused_as_the_argument_it_is` — Unexpected return code, failed var == 2
- RED `a_checkout_whose_path_needs_quoting_is_named_in_a_command_that_still_runs` — Unexpected failure.
- RED `every_state_a_branch_can_be_in_has_a_verb_that_takes_it` — Unexpected failure.
- RED `recovery_hands_a_hosted_identitys_complete_branch_to_the_verb_that_can_publish_it` — Unexpected failure.

### `02-the-train-routes-nowhere`

integrate's team and remote refusals go back to naming no command.

- RED `the_train_refuses_an_identity_whose_changes_are_reviewed` — Unexpected stderr, failed var.contains(`onevcs publish-branch claude/one --repo /tmp/.tmpmE3NhP/reviewed`)
- RED `a_train_refuses_a_single_owner_identity_that_publishes_through_its_host` — Unexpected stderr, failed var.contains(`onevcs publish-branch claude/one --repo /tmp/.tmpbbPhsO/remote-owner`)
- RED `every_state_a_branch_can_be_in_has_a_verb_that_takes_it` — the train's refusal names no command: onevcs: invalid input: direct integration is refused for identity "github.com/acme-corp/hosted" (repo_
- RED `a_checkout_whose_path_needs_quoting_is_named_in_a_command_that_still_runs` — Unexpected stderr, failed var.contains(`onevcs publish-branch feature/spacey --repo '/tmp/.tmpflExb7/a checkout with spaces'`)

### `03-recovery-hands-every-branch-to-the-train`

recover's handoff names `integrate` whatever the identity is, which is the verb half of them refuse.

- RED `recovery_hands_a_hosted_identitys_complete_branch_to_the_verb_that_can_publish_it` — the handoff names no command: onevcs: invalid input: branch "feature/handed-over" carries no unattested incomplete provenance: it has commit
- RED `a_checkout_whose_path_needs_quoting_is_named_in_a_command_that_still_runs` — Unexpected stderr, failed var.contains(`onevcs publish-branch feature/spacey --repo '/tmp/.tmpcXpCNI/a checkout with spaces'`)

### `04-interrupted-work-published-as-finished`

publish-branch stops refusing a branch that carries an unattested incomplete marker.

- RED `publishing_a_branch_refuses_interrupted_work_and_names_the_verb_that_lands_it` — Unexpected return code, failed var == 2

### `05-an-unreadable-marker-read-as-no-marker`

publish-branch stops looking for a marker written under a prefix this host cannot read.

- RED `a_marker_under_an_unreadable_prefix_is_never_published_as_a_finished_branch` — Unexpected return code, failed var == 2

### `06-a-branch-conflict-names-no-way-out`

the branch-keyed sync conflict goes back to stating that the two conflict.

- RED `a_change_base_that_conflicts_is_refused_once_and_lands_after_it_is_resolved` — Unexpected stderr, failed var.contains(re-running will conflict again)
- RED `a_recovery_whose_base_conflicts_keeps_the_preserved_branch` — Unexpected stderr, failed var.contains(re-running will conflict again)

### `07-a-session-conflict-names-no-way-out`

the publication path's sync conflict goes back to stating that the branch is retained.

- RED `a_base_that_conflicts_with_the_branch_reports_its_own_exit_code` — Unexpected stderr, failed var.contains(land it with `onevcs publish-branch feature/conflicting --repo /tmp/.tmpD3yGTI/project`)

### `08-printed-commands-are-not-quoted`

an argument a shell would split is printed as it is.

- RED `a_checkout_whose_path_needs_quoting_is_named_in_a_command_that_still_runs` — Unexpected stderr, failed var.contains(`onevcs publish-branch feature/spacey --repo '/tmp/.tmpW0jx1d/a checkout with spaces'`)

### `09-a-repository-path-read-lossily`

--repo is rendered through a lossy conversion instead of being refused when it is not text.

- RED `a_repository_path_that_is_not_text_is_refused_as_the_argument_it_is` — Unexpected stderr, failed var.contains(is not valid UTF-8)

### `10-the-subject-is-checked-after-the-attestation`

recover stops asking whether a subject exists before it writes to the branch.

- RED `a_title_publishes_a_recovery_whose_own_subjects_are_all_too_long` — Unexpected failure.

### `11-an-explicit-title-is-dropped`

the branch-keyed verbs stop carrying --title into the publication.

- RED `an_explicit_title_is_the_subject_a_branch_publishes_under` — assertion `left == right` failed
- RED `a_branch_with_no_usable_subject_is_refused_until_a_title_names_the_change` — Unexpected failure.
- RED `a_title_publishes_a_recovery_whose_own_subjects_are_all_too_long` — Unexpected failure.

### `12-a-per-run-policy-is-ignored`

--policy stops narrowing the policy the rules resolved.

- RED `a_per_run_policy_narrows_the_rules_resolved_one_and_never_widens_it` — Unexpected return code, failed var == 2

### `13-the-gate-does-not-run`

a publication stops running the gate the policy names.

- RED `a_gate_that_rejects_a_branch_keeps_it_where_it_was_found` — Unexpected return code, failed var == 1

### `14-an-identity-with-no-bar-is-only-diagnosed`

the refusal that nothing would be attested stops naming the rules entry that answers it.

- RED `an_identity_with_no_bar_is_told_which_rules_entry_would_give_it_one` — Unexpected stderr, failed var.contains(/tmp/.tmpRElobf/.onevcs/rules.yml)

### `15-a-recorded-base-is-refused-without-naming-it`

an unusable stack pointer is refused as a bare name rather than as the trailer it came from.

- RED `a_recorded_base_that_is_not_a_branch_names_the_trailer_that_says_so` — Unexpected stderr, failed var.contains(Onevcs-Change-Base:)

### `16-the-trains-arguments-are-only-diagnosed`

integrate's argument refusals go back to stating what is wrong and nothing else.

- RED `a_train_offered_something_it_cannot_run_says_which_and_why` — Unexpected stderr, failed var.contains(`onevcs recoverable`)
- RED `a_train_asked_to_push_a_checkout_with_no_origin_says_what_to_run_instead` — Unexpected stderr, failed var.contains(re-run `onevcs integrate` without --push)

### `17-a-branch-nobody-has-is-only-diagnosed`

the branch-keyed refusals stop naming the command that lists the branches there are.

- RED `a_branch_no_checkout_has_is_refused_by_the_command_that_lists_the_ones_that_do` — Unexpected stderr, failed var.contains(`onevcs recoverable`)
- RED `recovering_a_branch_no_checkout_has_names_everywhere_it_looked` — Unexpected stderr, failed var.contains(`onevcs recoverable`)
- RED `recovering_a_branch_with_nothing_ahead_of_its_base_says_there_is_nothing_to_recover` — Unexpected stderr, failed var.contains(`onevcs recoverable` lists the branches that do carry unpublished work)

### `18-a-skipped-candidate-is-handed-to-nobody`

the train's no-subject skip goes back to reporting the synthesis failure alone.

- RED `a_candidate_whose_content_the_base_already_carries_adds_no_second_commit` — Unexpected stdout, failed var.contains(publish it with `onevcs publish-branch claude/at-the-base --repo /tmp/.tmphNHZXe/project --title <T>`

### `19-the-verb-is-not-written-down`

the command surface record stops naming publish-branch, which is the drift the two readers exist to catch.

- RED `the_contract_and_clap_name_the_same_commands` — assertion `left == right` failed: docs/contract.md and the parser disagree about the command surface

