# Red, then green

Every journey this branch adds, observed failing for the behaviour it is
about before it passed. Regenerate with `just red-green`, which re-applies
each mutation under `scripts/red-green/`, records the assertion the test
failed on, reverts it, and then runs the same tests green.

Patches: 87. Tests observed red and then green: 110.

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

- RED `the_train_refuses_an_identity_whose_changes_are_reviewed` — the refusal routes claude/one nowhere:
- RED `a_train_refuses_a_single_owner_identity_that_publishes_through_its_host` — Unexpected stderr, failed var.contains(`onevcs publish-branch claude/one --repo <tmp>/remote-owner`)
- RED `every_state_a_branch_can_be_in_has_a_verb_that_takes_it` — the train's refusal names no command: onevcs: invalid input: direct integration is refused for identity "github.com/acme-corp/hosted" (repo_
- RED `a_checkout_whose_path_needs_quoting_is_named_in_a_command_that_still_runs` — Unexpected stderr, failed var.contains(`onevcs publish-branch feature/spacey --repo '<tmp>/a checkout with spaces'`)

### `03-recovery-hands-every-branch-to-the-train`

recover's handoff names `integrate` whatever the identity is, which is the verb half of them refuse.

- RED `recovery_hands_a_hosted_identitys_complete_branch_to_the_verb_that_can_publish_it` — the handoff names no command: onevcs: invalid input: branch "feature/handed-over" carries no unattested incomplete provenance: it has commit
- RED `a_checkout_whose_path_needs_quoting_is_named_in_a_command_that_still_runs` — Unexpected stderr, failed var.contains(`onevcs publish-branch feature/spacey --repo '<tmp>/a checkout with spaces'`)

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

- RED `a_base_that_conflicts_with_the_branch_reports_its_own_exit_code` — Unexpected stderr, failed var.contains(land it with `onevcs publish-branch feature/conflicting --repo <tmp>/project`)

### `08-printed-commands-are-not-quoted`

an argument a shell would split is printed as it is.

- RED `a_checkout_whose_path_needs_quoting_is_named_in_a_command_that_still_runs` — Unexpected stderr, failed var.contains(`onevcs publish-branch feature/spacey --repo '<tmp>/a checkout with spaces'`)

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

- RED `an_identity_with_no_bar_is_told_which_rules_entry_would_give_it_one` — Unexpected stderr, failed var.contains(<tmp>/.onevcs/rules.yml)

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

- RED `a_candidate_whose_content_the_base_already_carries_adds_no_second_commit` — Unexpected stdout, failed var.contains(publish it with `onevcs publish-branch claude/at-the-base --repo <tmp>/project --title <T>`)

### `19-the-verb-is-not-written-down`

the command surface record stops naming publish-branch, which is the drift the two readers exist to catch.

- RED `the_contract_and_clap_name_the_same_commands` — assertion `left == right` failed: the parser and the two documents that write the command surface down — docs/contract.md and docs/inferre

### `20-a-round-header-is-not-checked`

the harness stops checking a mutation patch header before it runs the round.

- RED `a_round_with_no_one_subject_is_refused_before_any_of_them_runs` — expected a non-zero exit:
- RED `a_round_that_names_no_test_or_names_one_twice_is_refused` — expected a non-zero exit:

### `21-a-round-is-not-put-back`

the harness stops reverting a round once it is over.

- RED `a_round_is_recorded_and_the_tree_is_left_as_it_was_found` — Unexpected failure: subject.txt says mutated

### `22-a-dirty-tree-is-mutated-anyway`

the harness stops refusing a tree that carries uncommitted work.

- RED `a_tree_or_a_log_the_harness_cannot_safely_use_stops_it_before_any_round` — stderr does not report "the working tree has uncommitted changes":

### `23-a-patch-is-taken-on-trust`

the harness stops checking that a patch is one git can read and apply.

- RED `a_patch_git_cannot_read_or_apply_stops_the_run_naming_it` — stderr does not report "is not a patch git can read":

### `24-a-round-that-observed-nothing-is-counted`

the harness stops caring whether the test a round names exists, or goes red.

- RED `a_round_naming_a_test_that_does_not_exist_or_does_not_go_red_puts_the_tree_back` — stderr does not report "names a test that does not exist":

### `25-the-green-half-is-never-run`

the harness stops running the tests with the behaviour back in place.

- RED `a_test_that_is_red_with_the_behaviour_in_place_fails_the_run` — expected a non-zero exit:

### `26-a-new-test-no-round-covers-is-let-through`

the harness stops holding this branch's new tests to a round that breaks them.

- RED `a_test_added_since_the_base_that_no_mutation_breaks_fails_the_run` — expected a non-zero exit:

### `27-the-base-is-taken-on-trust`

the harness stops checking that the base it was given can be read.

- RED `a_base_the_run_cannot_read_is_refused_where_it_is_named` — expected a non-zero exit:

### `28-a-tree-that-cannot-be-put-back-is-shrugged-at`

the harness reverts best-effort, so a tree left carrying a mutation says nothing.

- RED `a_tree_the_run_cannot_put_back_is_the_one_failure_it_says_loudest` — stderr does not report "the mutated files could not be restored":

### `29-a-transcript-nobody-could-write-is-not-noticed`

the harness stops checking that the transcript it was asked for can be written.

- RED `a_transcript_that_cannot_be_written_is_a_failure_rather_than_a_silent_omission` — stderr does not report "the transcript could not be written to nowhere/at/all.md":

### `30-a-smoke-refusal-names-no-next-step`

the release smoke script's state-root failure goes back to restating the invariant.

- RED `a_state_root_the_binary_cannot_read_is_reported_with_what_to_do_about_it` — the failure must name what to do about it:

### `31-a-painted-summary-is-read-as-paint`

the harness reads a runner's answer with whatever paint is on it.

- RED `a_runner_that_paints_its_summary_is_still_read_as_a_verdict` — expected success, got exit status: 1:

### `32-a-refusal-is-read-in-a-shell-that-cannot-print-it`

the harness goes back to a builtin the shell macOS ships does not have.

- RED `the_harness_still_refuses_where_the_shell_has_no_bash_four_builtins` — stderr does not report "no-such-ref does not name a commit here":
- RED `no_script_reaches_for_something_the_shell_macos_ships_does_not_have` — macOS ships bash 3.2, where a script that reaches for one of these aborts before the diagnostic it exists to print:

### `33-work-left-in-a-run-clone-is-not-searched-for`

the search for an identity stops at its registered checkouts.

- RED `work_a_stopped_run_left_only_in_its_clone_is_reported_and_landed_by_the_command_named` — the run clone is where the branch is: No preserved unpublished branches in <tmp>/project — the identity of <tmp>/project, the registered c

### `34-the-command-a-row-names-cannot-land-it`

finished work is offered to the local train instead of the verb that lands one branch.

- RED `recoverable_offers_each_preserved_branch_the_verb_its_provenance_earns` — assertion `left == right` failed
- RED `work_a_stopped_run_left_only_in_its_clone_is_reported_and_landed_by_the_command_named` — assertion `left == right` failed: 1 preserved unpublished branch(es) in <tmp>/project — the identity of <tmp>/project, the registered chec
- RED `session_events_match_across_backends` — assertion `left == right` failed: Git offers finished work the verb that lands one branch

### `35-a-scoped-answer-reads-as-the-whole-hosts`

the report does not say which identity it answered for.

- RED `a_scoped_recoverable_answer_names_the_identity_it_covers` — Unexpected stdout, failed var.contains(No preserved unpublished branches in)

### `36-a-pin-is-cut-fresh-and-says-nothing`

a pinned branch name is taken on trust.

- RED `a_branch_pin_the_session_could_not_carry_is_refused_rather_than_cut_fresh` — Unexpected return code, failed var == 2

### `37-a-spent-name-answers-for-a-live-one`

the first repository to hold a name answers for it, spent or not.

- RED `a_name_the_checkout_has_spent_does_not_answer_for_the_run_clone_that_reuses_it` — No preserved unpublished branches. Every branch across the registered identities has reached its base or a remote.

### `38-a-spent-copy-of-a-name-is-published`

a branch is located by name alone, spent copy or not.

- RED `a_name_the_checkout_has_spent_does_not_answer_for_the_run_clone_that_reuses_it` — Unexpected stdout, failed var.contains(merged at)

### `39-a-base-nobody-can-reach-judges-the-branch`

a base the repository cannot reach is used to judge it anyway.

- RED `a_run_clone_that_cannot_reach_the_base_is_judged_against_the_one_it_can` — Unexpected failure.

### `40-the-callers-body-is-dropped`

a publication stops carrying the caller's body into the change request it opens.

- RED `a_requested_body_is_what_the_change_request_is_opened_with_verbatim` — assertion `left == right` failed
- RED `the_command_line_gives_a_change_request_its_body_as_text_or_as_a_file` — assertion `left == right` failed

### `41-a-body-is-composed-when-nobody-passed-one`

the composed scaffold comes back for a publication that was given no body.

- RED `a_publication_given_no_body_opens_a_change_request_with_no_body_at_all` — a publication that was given no body composes none: "## What\n\nfeat: the thing that describes itself\n\n## Why\n\nPublished by onevcs.\n"
- RED `a_recovered_change_request_carries_its_attestation_on_the_branch_and_no_body_at_all` — assertion `left == right` failed: a recovery composes no body either
- RED `a_complete_branch_of_a_team_identity_opens_the_change_request_its_rules_require` — assertion `left == right` failed: a branch-keyed publication composes no body

### `42-two-bodies-are-accepted`

naming the body twice stops being refused, and one of the two is silently taken.

- RED `naming_the_body_twice_is_refused_by_name_before_anything_is_published` — Unexpected return code, failed var == 2

### `43-the-provider-drops-the-callers-body`

the testing repository stops handing the caller's body to the host it publishes through.

- RED `a_requested_title_and_body_are_the_ones_the_host_is_given` — no entry found for key

### `44-an-option-publish-takes-is-not-written-down`

the amendment stops naming one of the options `onevcs publish` takes.

- RED `the_amendment_names_every_option_publish_takes_that_the_approved_usage_does_not` — assertion `left == right` failed: the amendment and `onevcs publish` disagree about which options it takes beyond the approved two

### `45-a-recovery-attests-nothing`

recovery stops writing the attestation commit that carries the cleared marker forward.

- RED `a_recovered_change_request_carries_its_attestation_on_the_branch_and_no_body_at_all` — the pushed branch must carry the recovery forward:

### `46-the-previous-version-is-refused-rather-than-read`

the readable range narrows back to one version, so a scenario written by the build before this one is refused.

- RED `a_document_at_the_previous_version_is_read_and_written_back_at_this_one` — the previous version reads: Invalid { reason: "the provider state at <tmp>/host.json: invalid input: the document declares version 2; this b

### `47-a-carried-forward-document-keeps-the-version-it-arrived-at`

a document read at an older version is not carried forward, so the next write declares a version it is not.

- RED `a_document_at_the_previous_version_is_read_and_written_back_at_this_one` — assertion `left == right` failed

### `48-the-version-floor-is-not-a-floor`

the oldest readable version stops being a floor, so a version 1 document is read instead of refused.

- RED `a_document_declaring_a_version_this_build_does_not_read_is_refused_by_name` — a document at version 1 is one nothing here reads

### `49-an-added-field-is-written-even-when-it-holds-nothing`

the body map is written whether or not it holds anything, so a document names a field no scenario asked for.

- RED `an_empty_state_is_written_as_its_golden_and_omits_every_field_it_does_not_hold` — assertion `left == right` failed: the host state a fresh provider writes is its checked-in golden

### `50-an-empty-body-is-recorded-as-no-body`

the testing host stops telling an empty body from an absent one, and records neither.

- RED `a_change_request_records_the_body_it_was_opened_with_and_none_when_it_had_none` — no entry found for key

### `51-a-body-about-a-change-nobody-opened-is-carried`

a seeded body about a change request nobody opened stops being refused.

- RED `a_seeded_document_holding_a_session_nothing_could_act_on_is_refused` — a record about a change nobody opened: Host { store: FileStore { path: "<tmp>/host.json", marker: PhantomData<onevcs_testing::state::HostSta

### `52-the-record-names-an-option-publish-does-not-take`

the inferred-surface record's field list stops matching the request a publication takes.

- RED `the_inferred_surface_row_lists_the_fields_publish_request_actually_has` — assertion `left == right` failed: docs/inferred-surface.md and PublishRequest disagree about which options a publication takes

### `53-a-landed-stack-parent-is-merged-again`

a root that already carries the change below a stack is merged into like anything else.

- RED `a_recorded_stack_that_squash_merged_is_replayed_onto_the_root_rather_than_merged` — Unexpected failure.
- RED `a_conflict_in_a_replayed_branchs_own_work_is_refused_with_the_replay_that_lands_it` — Unexpected return code, failed var == 3
- RED `a_stack_whose_paths_this_process_cannot_read_is_answered_by_content_alone` — Unexpected failure.
- RED `a_recovery_whose_recorded_stack_already_landed_is_replayed_onto_the_root` — Unexpected failure.
- RED `a_recoverys_replay_conflict_keeps_the_branch_and_names_the_replay` — Unexpected return code, failed var == 3
- RED `a_publish_branch_whose_recorded_stack_already_landed_is_replayed_onto_the_root` — Unexpected failure.
- RED `a_hosted_stack_whose_change_below_landed_opens_its_review_against_the_root` — Unexpected failure.
- RED `a_hosted_stack_the_root_independently_matches_is_answered_the_same_way` — assertion `left == right` failed: [Object {"v": Number(1), "ts": String("<time>"), "stream": String("<token>"), "seq": Number(5), "source": 
- RED `a_publish_branch_replay_conflict_names_its_own_command` — Unexpected return code, failed var == 3
- RED `a_root_that_advances_after_the_gate_is_resynced_without_the_stack_returning` — Unexpected failure.

### `54-a-stack-inferred-from-content`

a branch is stacked whenever its content looks stacked, rather than when its record says so.

- RED `a_branch_the_base_independently_matches_is_still_merged_because_no_record_stacks_it` — assertion `left == right` failed: the base arrived as the merge it always arrives as

### `55-an-unreadable-path-list-is-read-as-no-change`

a path list this process could not take whole is answered as a commit that changed nothing.

- RED `a_stack_whose_paths_this_process_cannot_read_is_answered_by_content_alone` — Unexpected failure.

### `56-a-stack-is-never-written-down`

the tip a session was cut from is never written down, so no session records a stack.

- RED `a_stacked_session_records_the_tip_it_was_cut_from_and_keeps_it_through_its_life` — assertion `left == right` failed: {"version":3,"token":"<token>","identity":"<tmp>/project","alias":"project","branch":"feature/above","base

### `57-a-recorded-stack-is-taken-on-trust`

every check on a recorded stack past the record itself is dropped.

- RED `a_root_this_clone_no_longer_has_leaves_the_stack_where_it_is` — Unexpected failure.
- RED `a_branch_that_left_its_recorded_stack_behind_is_merged_rather_than_replayed` — assertion `left == right` failed
- RED `a_stack_merged_with_its_own_commits_keeps_targeting_the_stack` — assertion `left == right` failed
- RED `a_stack_that_shares_no_history_with_the_root_keeps_targeting_the_stack` — assertion `left == right` failed

### `58-a-root-nobody-can-name-is-guessed`

a root the publication checkout cannot name is guessed at instead.

- RED `a_root_the_publication_checkout_cannot_name_leaves_the_stack_where_it_is` — Unexpected failure.

### `59-a-vanished-recorded-base-is-handed-to-git`

a recorded base nothing resolves is handed to git as a revision instead of refused.

- RED `a_recorded_base_no_ref_resolves_names_what_would_restore_it` — Unexpected stderr, failed var.contains(records the base it was stacked on as "feature/gone")
- RED `a_publish_branch_whose_recorded_base_no_ref_resolves_names_its_own_command` — Unexpected stderr, failed var.contains(records the base it was stacked on as "feature/gone")

### `60-an-unreadable-stack-tip-is-read-as-no-stack`

a recorded stack tip this clone does not have is read as no stack at all.

- RED `a_recorded_stack_tip_this_clone_does_not_have_is_refused_by_name` — Unexpected stderr, failed var.contains(names "0000000000000000000000000000000000000000" as the commit branch "feature/above" was cut from)

### `61-a-renames-source-is-never-compared`

a rename is compared under the name git reports it as, and not under the one it left.

- RED `a_stack_that_renamed_a_file_the_root_still_has_keeps_targeting_the_stack` — assertion `left == right` failed

### `62-a-recorded-path-is-this-runs-own`

a recorded diagnostic keeps the values only this run would spell.

- RED `a_recorded_diagnostic_carries_no_path_a_second_run_would_spell_differently` — every value only this run would spell is a placeholder, and what it said is not:

### `63-a-moved-base-is-not-re-judged`

a base that moved after the gate is neither re-synced nor re-judged.

- RED `a_root_that_advances_after_the_gate_is_resynced_without_the_stack_returning` — assertion `left == right` failed: the gate judged the base it landed on, not only the base it started from

### `64-an-unreadable-listing-is-read-as-carried`

a listing that did not arrive whole is answered as content the base carries.

- RED `an_unreadable_listing_and_a_root_that_moved_on_leaves_the_stack_where_it_is` — Unexpected failure.

### `65-a-root-nobody-can-name-refuses-the-session`

a session naming its own base is refused when nothing can name the identity's root.

- RED `a_session_whose_root_nobody_can_name_records_no_stack_and_publishes_as_one` — Unexpected failure.

### `66-a-replayed-branch-is-pushed-as-a-fast-forward`

a branch whose history a replay rewrote is pushed as though it had not been.

- RED `a_review_opened_against_the_change_below_is_reopened_against_the_root_once_it_lands` — Unexpected failure.
- RED `a_branch_the_host_moved_under_a_replay_is_refused_without_overwriting_it` — Unexpected return code, failed var == 3
- RED `a_recovery_that_replays_a_branch_the_host_has_replaces_it_there` — Unexpected failure.

### `67-a-lease-names-the-hosts-own-commit`

the lease names whatever the host has now, rather than the commit this run replaces.

- RED `a_branch_the_host_moved_under_a_replay_is_refused_without_overwriting_it` — Unexpected return code, failed var == 3

### `68-a-refused-push-is-read-as-a-moved-branch`

every push a leased publication has refused is read as a branch somebody moved.

- RED `a_replays_push_the_merge_path_rejects_is_reported_as_the_rejection_it_is` — Unexpected return code, failed var == 1
- RED `a_leased_push_the_host_refuses_is_read_from_what_git_reports_and_not_from_its_wording` — Unexpected return code, failed var == 1
- RED `a_leased_push_no_host_is_left_to_answer_for_is_reported_as_the_rejection_it_is` — Unexpected return code, failed var == 1

### `69-a-declined-lease-is-read-out-of-the-prose`

whether the lease was declined, and what the host has instead, is read out of git's diagnostic prose.

- RED `a_leased_push_the_host_refuses_is_read_from_what_git_reports_and_not_from_its_wording` — Unexpected return code, failed var == 1
- RED `a_branch_the_host_moved_before_a_recovery_was_invoked_is_refused_without_overwriting_it` — the lease named the commit this run saw, and the refusal says where the host is now:
- RED `a_branch_deleted_on_the_host_under_a_replay_is_refused_as_the_branch_that_is_gone` — Unexpected stderr, failed var.contains("feature/filter" is gone from the host, which this run last had at)

### `70-a-lease-taken-from-what-the-fetch-found`

the lease is taken from what this run's own fetch found on the host.

- RED `a_branch_the_host_moved_before_a_recovery_was_invoked_is_refused_without_overwriting_it` — Unexpected return code, failed var == 3
- RED `a_replay_of_a_branch_this_run_has_never_seen_on_the_host_is_refused_before_it_pushes` — Unexpected return code, failed var == 3

### `71-a-host-copy-this-run-never-saw-is-pushed-over`

a host copy this run has never seen is pushed over rather than refused.

- RED `a_replay_of_a_branch_this_run_has_never_seen_on_the_host_is_refused_before_it_pushes` — Unexpected return code, failed var == 3

### `72-a-branch-gone-from-the-host-declined-nothing`

a branch deleted on the host is a rejection nothing classified, rather than the lease it declined.

- RED `a_branch_deleted_on_the_host_under_a_replay_is_refused_as_the_branch_that_is_gone` — Unexpected return code, failed var == 3

### `73-a-host-that-would-not-say-is-read-as-one-with-no-branch`

a host that would not say where the branch is is read as one that has no such branch.

- RED `a_leased_push_no_host_is_left_to_answer_for_is_reported_as_the_rejection_it_is` — Unexpected return code, failed var == 1

### `74-a-change-request-url-is-not-a-reference`

status resolves a change request's URL as no reference at all, which is the state before this branch: the URL is the host's name for the work and nothing here reads it.

- RED `every_spelling_of_one_piece_of_work_resolves_to_the_same_report` — Unexpected failure.
- RED `an_ambiguous_reference_is_refused_by_naming_the_candidates` — Unexpected stderr, failed var.contains(is a name git would not accept)
- RED `landing_is_told_apart_from_a_queued_merge_and_from_a_change_that_closed` — Unexpected failure.

### `75-an-ambiguous-reference-answers-with-the-first-candidate`

status answers about whichever piece of work a reference matched first, rather than refusing and naming them.

- RED `an_ambiguous_reference_is_refused_by_naming_the_candidates` — Unexpected return code, failed var == 2

### `76-a-branch-a-session-still-holds-is-handed-to-the-verb-for-preserved-work`

status stops noticing that an open session holds the branch, so it names the verb that publishes work nobody holds.

- RED `work_a_run_left_in_its_own_clone_is_reported_with_the_verb_that_lands_it` — assertion `left == right` failed: an open session's branch is published through the session

### `77-a-base-that-carries-the-work-is-read-as-unpublished`

status stops reading a landing off the base's content, which is the answer a squash-merge leaves and the one a planner got wrong.

- RED `landing_is_told_apart_from_a_queued_merge_and_from_a_change_that_closed` — assertion `left == right` failed

### `78-a-host-that-could-not-be-asked-is-read-as-one-with-nothing-to-say`

status reports a host it could not reach as a host that answered there is nothing, so a gap in the report reads as an answer.

- RED `a_host_that_cannot_be_asked_leaves_its_section_unavailable_and_answers_the_rest` — assertion `left == right` failed

### `79-a-source-nobody-named-is-not-searched-for`

import stops searching everywhere the identity keeps work when nobody passed --from, which is every branch a run left in its own clone.

- RED `a_branch_only_a_run_clone_has_is_imported_without_touching_any_working_tree` — Unexpected failure.
- RED `a_spent_name_does_not_block_an_import_under_another` — Unexpected failure.

### `80-a-name-the-destination-has-checked-out-is-written-anyway`

import stops refusing the one name a checkout has checked out, so a ref write leaves a working tree describing a commit the branch no longer names.

- RED `a_branch_only_a_run_clone_has_is_imported_without_touching_any_working_tree` — Unexpected return code, failed var == 2

### `81-a-remote-ref-is-not-a-source`

import stops taking a remote ref as a source, leaving a branch only the origin has reachable by no verb.

- RED `a_branch_is_imported_from_another_checkout_and_from_a_remote_ref` — Unexpected failure.

### `82-an-alternate-name-is-dropped`

import writes the branch's own name whatever --as asked for, so work whose name is already spent has nowhere to go.

- RED `a_spent_name_does_not_block_an_import_under_another` — Unexpected stdout, failed var.contains(imported preserved/held)
- RED `a_branch_only_a_run_clone_has_is_imported_without_touching_any_working_tree` — Unexpected return code, failed var == 2

### `83-a-name-is-overwritten-with-whatever-arrives`

import stops refusing a non-fast-forward, so a name a checkout holds work under is written over by whatever the source has.

- RED `an_import_that_would_not_fast_forward_is_refused_naming_what_it_would_lose` — Unexpected return code, failed var == 2

### `84-every-gate-verdict-is-read-as-a-pass`

status stops reading what a gate-verdict event actually said, so a refusal and a word this build does not know both report as a pass.

- RED `the_last_gate_verdict_recorded_for_the_work_is_what_the_report_names` — assertion `left == right` failed

### `85-a-branch-a-stream-names-is-taken-on-trust`

status hands a stream's recorded branch to git without asking whether it is a branch name at all.

- RED `an_ambiguous_reference_is_refused_by_naming_the_candidates` — Unexpected stderr, failed var.contains(is a name git would not accept)

### `86-the-report-does-not-say-which-shape-it-is`

the status report stops declaring its schema version, so a consumer reads a shape it inferred from the keys it could find.

- RED `the_status_report_is_the_versioned_object_its_goldens_record` — assertion `left == right` failed: the report declares the version the surface record documents

### `87-a-field-that-holds-nothing-is-written-as-null`

the status report writes an optional field even when it holds nothing, so a consumer that never heard of a session is handed one that is null.

- RED `the_status_report_is_the_versioned_object_its_goldens_record` — assertion `left == right` failed: the object a report with nothing optional in it writes is its checked-in golden; re-make crates/onevcs/tes

