# Red, then green

Every journey this branch adds, observed failing for the behaviour it is
about before it passed. Regenerate with `just red-green`, which re-applies
each mutation under `scripts/red-green/`, records the assertion the test
failed on, reverts it, and then runs the same tests green.

Patches: 229. Tests observed red and then green: 264.

### `01-the-verb-has-no-implementation`

publish-branch answers NotImplemented, which is the state before this branch: the verb parses and nothing is behind it.

- RED `a_complete_branch_of_a_local_identity_lands_on_its_base` — Unexpected failure.
- RED `a_complete_branch_of_a_team_identity_opens_the_change_request_its_rules_require` — Unexpected failure.
- RED `a_complete_branch_of_a_remote_identity_is_landed_by_the_host` — Unexpected failure.
- RED `a_branch_the_host_holds_is_watched_until_it_lands` — Unexpected failure.
- RED `a_hosted_origin_this_build_does_not_speak_for_answers_the_seam_it_has_no_body_for` — Unexpected stderr, failed var.contains(RemoteHost for a host other than github.com is not implemented yet)
- RED `an_identity_with_no_rules_file_publishes_under_the_built_in_default` — Unexpected failure.
- RED `publishing_a_branch_refuses_interrupted_work_and_names_the_verb_that_lands_it` — Unexpected return code, failed var == 2
- RED `a_branch_no_checkout_has_is_refused_by_the_command_that_lists_the_ones_that_do` — Unexpected return code, failed var == 2
- RED `an_explicit_title_is_the_subject_a_branch_publishes_under` — Unexpected failure.
- RED `a_branch_with_no_usable_subject_is_refused_until_a_title_names_the_change` — Unexpected return code, failed var == 2
- RED `a_per_run_policy_narrows_the_rules_resolved_one_and_never_widens_it` — Unexpected return code, failed var == 2
- RED `a_merge_path_that_rejects_a_branch_keeps_it_where_it_was_found` — Unexpected return code, failed var == 1
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

- RED `a_base_that_conflicts_with_the_branch_reports_its_own_exit_code` — Unexpected stderr, failed var.contains(in "shared.txt")

### `08-printed-commands-are-not-quoted`

an argument a shell would split is printed as it is.

- RED `a_checkout_whose_path_needs_quoting_is_named_in_a_command_that_still_runs` — Unexpected stderr, failed var.contains(`onevcs publish-branch feature/spacey --repo '<tmp>/a checkout with spaces'`)

### `09-a-repository-path-read-lossily`

--repo is rendered through a lossy conversion instead of being refused when it is not text.

- RED `a_repository_path_that_is_not_text_is_refused_as_the_argument_it_is` — Unexpected stderr, failed var.contains(is not valid UTF-8)

### `100-a-copy-is-taken-rather-than-compared`

the copies of a continued branch stop being compared — a checkout's is taken whatever origin holds.

- RED `a_continued_branch_opens_at_whichever_copy_carries_the_other` — the session opens at origin's copy, which carries this checkout's
- RED `copies_of_a_continued_branch_that_diverged_are_refused_rather_than_one_being_chosen` — Unexpected return code, failed var == 2

### `101-a-branch-may-be-its-own-base-again`

a session may name its own branch as its base again, and publish it into itself.

- RED `a_session_whose_base_is_its_own_branch_is_refused_naming_the_spelling_that_replaced_it` — Unexpected return code, failed var == 2

### `102-a-continued-session-conflict-names-no-way-out`

the refusal a continued branch's conflict gives names neither the copy it is in nor the command that lands it.

- RED `a_continued_branch_whose_base_conflicts_is_refused_naming_where_it_is_and_what_lands_it` — Unexpected stderr, failed var.contains(in "shared.txt")

### `103-a-run-root-somebody-is-working-in-is-read-as-abandoned`

the occupancy lease on a session's run root stops being consulted, so a command working in there reads as nobody.

- RED `a_branch_a_live_session_still_holds_is_not_offered_as_ready_to_land` — assertion `left == right` failed: [

### `104-a-session-whose-owner-is-still-running-is-read-as-stopped`

a session whose own process is still running stops counting as holding its branch, so a consumer embedding the crate is offered its own live work.

- RED `a_branch_the_calling_process_still_holds_is_reported_as_held_rather_than_ready` — a live session's hold is reported: Recoverable {

### `105-a-held-branch-is-offered-to-be-resumed-anyway`

the row of a branch a live session holds is printed as ready to resume, with the same paste-ready command as every other row.

- RED `a_branch_a_live_session_still_holds_is_not_offered_as_ready_to_land` — a branch a live session holds is not offered to be resumed:

### `106-a-branch-that-strips-more-than-it-adds-is-reported-as-any-other`

a preserved branch's lines stop being weighed, so one that removes far more than it adds is reported exactly as a healthy one.

- RED `a_branch_that_removes_more_than_it_adds_is_marked_in_both_renderings` — assertion `left == right` failed: the lines it would land, counted from where it forked: [
- RED `what_the_net_negative_count_does_not_count_leaves_a_branch_unmarked` — assertion `left == right` failed: the count is of the lines git counted: [

### `107-a-diverged-pair-is-refused-without-saying-how-the-copies-differ`

the refusal a diverged pair produces stops saying how the two copies differ and stops printing the diff between them.

- RED `a_copy_amended_in_one_checkout_is_refused_naming_both_trees_and_how_they_differ` — the refusal says how the two differ:
- RED `copies_of_one_branch_that_have_diverged_refuse_the_landing_and_name_each_one` — the refusal says how the two copies differ:

### `108-a-file-with-no-line-count-is-read-as-a-number`

a file git compares as binary stops being left out of the line count, so its `-` is read as a number and the whole report fails on it.

- RED `what_the_net_negative_count_does_not_count_leaves_a_branch_unmarked` — Unexpected failure.

### `109-the-net-negative-boundary-takes-a-branch-that-is-even`

the net-negative boundary stops being strict, so a branch that adds exactly as much as it removes is a mark this type will hold.

- RED `what_the_net_negative_count_does_not_count_leaves_a_branch_unmarked` — feature/one-for-one is not net-negative: [
- RED `the_reported_shapes_serialize_the_way_a_json_consumer_reads_them` — (5, 5) added and removed is not a net-negative change

### `10-the-subject-is-checked-after-the-attestation`

recover stops asking whether a subject exists before it writes to the branch.

- RED `a_title_publishes_a_recovery_whose_own_subjects_are_all_too_long` — Unexpected failure.

### `110-a-branch-that-shares-no-history-is-measured-against-the-base-anyway`

a branch sharing no history with the base stops being left unmeasured, so it is compared with the base's tip and reads as removing everything the base has.

- RED `what_the_net_negative_count_does_not_count_leaves_a_branch_unmarked` — feature/unrelated is not net-negative: [

### `111-the-provider-answers-no-hold-for-a-session-it-still-has-open`

the provider stops answering which of its own open sessions holds a preserved branch, so a consumer's suite is offered work its session is still in.

- RED `preserved_work_is_what_recoverable_reports` — an open session still holds its branch: Recoverable {

### `112-a-hold-naming-a-session-nobody-opened-is-taken-on-trust`

a seeded row held by a session nobody opened stops being refused, so a provider answers a hold out of a session that never existed.

- RED `a_document_that_records_anything_about_a_session_nobody_opened_is_refused_by_name` — preserved work held by a session that was never opened describes a run nothing could have made

### `113-a-copys-parent-is-left-out-of-the-refusal`

the refusal stops naming the commit each copy stands on, so nothing says where the two forked apart.

- RED `a_copy_amended_in_one_checkout_is_refused_naming_both_trees_and_how_they_differ` — every fact about a copy is named against the checkout it came out of; this one is not:
- RED `copies_of_one_branch_that_have_diverged_refuse_the_landing_and_name_each_one` — the copy in <tmp>/.onevcs/workspaces/-tmp-.tmpKg55wx-project-d63751fe5c93/runs/<token>/clone stands on its own parent b89d289651db3bf10cdcf2

### `114-when-a-copy-was-committed-is-left-out-of-the-refusal`

the refusal stops naming when each copy was committed, so nothing says which of the two was taken second.

- RED `a_copy_amended_in_one_checkout_is_refused_naming_both_trees_and_how_they_differ` — every fact about a copy is named against the checkout it came out of; this one is not:
- RED `copies_of_one_branch_that_have_diverged_refuse_the_landing_and_name_each_one` — …and names when it was committed:

### `115-the-two-copies-facts-are-crossed`

each copy's facts are stated against the other copy's checkout, so every value is there and every one of them is attributed to the wrong tree.

- RED `a_copy_amended_in_one_checkout_is_refused_naming_both_trees_and_how_they_differ` — every fact about a copy is named against the checkout it came out of; this one is not:
- RED `copies_of_one_branch_that_have_diverged_refuse_the_landing_and_name_each_one` — the copy in <tmp>/.onevcs/workspaces/-tmp-.tmpWxZjj7-project-a3dd87b459df/runs/<token>/clone stands on its own parent f1de4a640b89625aced930

### `116-a-branch-only-origin-carries-is-not-looked-for`

the copy origin holds stops being one of the copies a pin is continued from, so a branch no checkout here has ever seen is cut fresh over it.

- RED `a_branch_pin_naming_work_that_already_exists_continues_it_rather_than_cutting_fresh` — the branch only origin carries is continued at the commit origin has
- RED `a_continued_branch_opens_at_whichever_copy_carries_the_other` — the session opens at origin's copy, which carries this checkout's

### `117-a-continued-session-says-it-cut-the-branch-fresh`

the opening of a continued session stops saying so, so a follower reading the stream cannot tell a session that found the work from one that started over.

- RED `a_branch_pin_naming_work_that_already_exists_continues_it_rather_than_cutting_fresh` — assertion `left == right` failed
- RED `a_continued_branch_opens_at_whichever_copy_carries_the_other` — assertion `left == right` failed: [Object {"v": Number(1), "ts": String("<time>"), "stream": String("<token>"), "seq": Number(2), "source": 

### `118-a-proven-dead-workspace-is-never-removed`

a sweep reports what it reclaimed and removes none of it, which is the state before this verb: the directories are named and every one of them is still there.

- RED `a_finished_publication_workspace_older_than_the_age_floor_is_reclaimed` — a publication that was gated and is nobody's is reclaimed:
- RED `a_recovery_workspace_is_reaped_by_the_same_verb_as_a_publication` — a recovery cuts the same shape of run root and is reaped by the same verb:
- RED `a_dry_run_reports_what_it_would_reclaim_and_removes_nothing` — the real run reclaims what the rehearsal named:
- RED `the_age_floor_bounds_what_a_sweep_considers` — a floor of nought considers what was written moments ago:

### `119-occupancy-is-asked-in-the-mode-that-cannot-answer-it`

the sweep asks for a run root's lease in the shared mode every occupant already holds it in, so a directory somebody is publishing in answers that nobody is.

- RED `a_publication_somebody_is_still_making_is_retained_and_nothing_about_it_is_terminated` — assertion `left == right` failed

### `11-an-explicit-title-is-dropped`

the branch-keyed verbs stop carrying --title into the publication.

- RED `an_explicit_title_is_the_subject_a_branch_publishes_under` — assertion `left == right` failed
- RED `a_branch_with_no_usable_subject_is_refused_until_a_title_names_the_change` — Unexpected failure.
- RED `a_title_publishes_a_recovery_whose_own_subjects_are_all_too_long` — Unexpected failure.

### `120-a-landing-leases-something-other-than-its-run-root`

a landing's occupancy lease names something other than the run root it works in, so nothing it holds says a publication is being made there.

- RED `a_publication_somebody_is_still_making_is_retained_and_nothing_about_it_is_terminated` — assertion `left == right` failed

### `121-a-merge-path-that-recorded-nothing-is-read-as-a-verdict`

a run root nothing ever judged answers that its merge path reached a verdict, so an unfinished publication is read as a finished one.

- RED `a_workspace_whose_merge_path_recorded_no_verdict_is_retained_with_that_reason` — assertion `left == right` failed

### `122-a-directory-nobody-can-vouch-for-is-taken-on-trust`

the sweep stops asking whether a directory under its families is one this crate cut, so somebody else's workspace is judged as if it were onevcs's own.

- RED `a_directory_this_verb_cannot_show_it_cut_is_retained_with_that_reason` — assertion `left == right` failed

### `123-a-rehearsal-removes-what-it-only-reports`

--dry-run stops being a rehearsal: the run that was asked to report what it would do does it.

- RED `a_dry_run_reports_what_it_would_reclaim_and_removes_nothing` — a rehearsal removes nothing:
- RED `reclaiming_a_workspace_stops_the_process_the_publication_left_running` — the rehearsal names the process the removal would reach for:

### `124-the-age-floor-is-never-consulted`

the age floor stops bounding what a sweep considers, so a workspace written moments ago is reaped by a sweep that was told to leave a day's work alone.

- RED `the_age_floor_bounds_what_a_sweep_considers` — the default floor retains it:
- RED `a_landing_reclaims_the_workspaces_the_landings_before_it_left_behind` — a workspace written minutes ago is inside the floor, whoever is asking

### `125-the-lifecycle-clone-root-is-reached-into`

the sweep reaches into the per-run lifecycle clone root, which is the bounded recovery history that keeps a dead run's branch reachable.

- RED `the_per_run_lifecycle_clones_are_a_family_this_verb_does_not_reach_into` — the report names the family it did not examine, and why:

### `126-an-age-floor-no-window-can-hold-is-accepted`

--min-age-hours takes a value no window can hold and answers with one anyway, so a floor nobody typed decides what is reaped.

- RED `an_age_floor_no_window_can_hold_is_refused_at_the_boundary` — Unexpected return code, failed var == 2

### `127-a-state-root-with-no-workspaces-is-a-failure`

a state root nothing has published from is read as a sweep that could not run, rather than as one with nothing to do.

- RED `a_state_root_nothing_has_published_from_is_a_sweep_with_nothing_to_do` — Unexpected failure.

### `128-the-report-stops-naming-what-it-did-not-examine`

the report stops naming the directories under its own root that it did not examine, so a reader cannot tell what was covered from what was passed over.

- RED `the_per_run_lifecycle_clones_are_a_family_this_verb_does_not_reach_into` — the report names the family it did not examine, and why:
- RED `what_is_under_the_root_and_is_not_a_run_root_is_reported_rather_than_touched` — the report names what it did not examine, and why:

### `129-a-run-root-that-is-not-a-directory-is-judged-as-one`

the sweep stops asking whether what it found is a directory at all, so a file left under a family is judged as a run root.

- RED `what_is_under_the_root_and_is_not_a_run_root_is_reported_rather_than_touched` — assertion `left == right` failed

### `12-a-per-run-policy-is-ignored`

--policy stops narrowing the policy the rules resolved.

- RED `a_per_run_policy_narrows_the_rules_resolved_one_and_never_widens_it` — Unexpected return code, failed var == 2

### `130-a-workspace-nobody-here-could-remove-is-taken-apart-to-find-out`

the sweep stops asking whether removing a workspace is this host's to do and finds out by trying, which destroys everything above the first thing it cannot unlink.

- RED `what_this_host_may_not_read_or_remove_is_reported_rather_than_failing_the_sweep` — nothing under a workspace this host may not remove was touched:

### `131-the-shared-age-floor-moves-on-its-own`

the age floor's default moves in the parser alone, which is how one caller forwarding one argument set to two tools comes to get two different windows.

- RED `the_sweep_age_floor_defaults_to_the_number_the_record_states` — assertion `left == right` failed: docs/inferred-surface.md states an age floor of "24" and the parser defaults to ["48"]; the number is shar

### `132-a-family-this-sweep-could-not-read-is-passed-over-in-silence`

a family of run roots this sweep could not even list is left out of the report, so a directory full of workspaces reads as one holding none.

- RED `what_this_host_may_not_read_or_remove_is_reported_rather_than_failing_the_sweep` — a family this sweep could not examine is named as one:

### `133-a-workspaces-age-is-read-off-the-top-of-it`

a run root's age is read from its own timestamp and its immediate entries alone, so a gate rewriting a file deep inside the clone leaves the workspace looking a day old.

- RED `the_age_floor_bounds_what_a_sweep_considers` — a workspace written inside a moment ago is not a day old:

### `134-a-probe-leaves-the-clock-it-moved`

the writability probe stops putting a directory's timestamps back, so every workspace it asks about looks freshly written and the age floor keeps it for another day.

- RED `what_this_host_may_not_read_or_remove_is_reported_rather_than_failing_the_sweep` — asking again gives the same answer, not one about the clock:

### `135-a-probe-writes-before-it-can-put-the-clock-back`

the writability probe writes into a directory before it has put the clock back once, so a directory whose timestamps it cannot set is left aged by having been asked about.

- RED `a_workspace_the_sweep_could_not_ask_about_is_not_aged_by_the_asking` — a workspace is as old as the work in it, not as old as the last sweep:

### `137-a-directory-that-hands-unlinks-to-owners-is-answered-for-anyway`

the sweep answers for a directory whose entries only their owners may unlink, so a workspace it cannot show it may empty is emptied on a permission that says nothing about it.

- RED `a_workspace_holding_a_directory_that_hands_unlinks_to_owners_is_retained` — a workspace holding one is kept rather than emptied on a permission that does not answer for it:

### `138-a-probe-entry-that-could-not-be-taken-away-is-shrugged-at`

the writability probe shrugs at an entry it could not take away again, so it goes on asking inside what it left behind rather than answering that it could not finish.

- RED `a_probe_entry_this_host_could_not_take_away_again_is_no_answer_about_the_workspace` — assertion `left == right` failed: and the asking stopped there rather than going on inside the entry it could not take away:
- RED `a_probe_file_this_host_could_not_unlink_is_no_answer_about_the_workspace` — Unexpected failure.

### `139-a-clock-that-cannot-be-put-back-is-no-obstacle`

the writability probe stops treating a clock it cannot put back as an answer, so it writes into a directory it cannot leave as it found and reports what it wrote in as removable.

- RED `a_directory_whose_clock_this_host_could_not_put_back_is_never_written_into` — a directory this could not leave as it found it is no answer, so nothing was removed:

### `140-a-landing-never-enforces-the-retention-rule`

a branch-keyed landing stops enforcing the retention rule over its own family, so the workspaces the landings before it left behind are reaped only when somebody remembers to run the verb.

- RED `a_landing_reclaims_the_workspaces_the_landings_before_it_left_behind` — the landing enforced the retention rule over its own family
- RED `a_landing_never_reclaims_a_workspace_somebody_holds_the_lease_on` — the workspace nobody is inside is reclaimed by the landing
- RED `a_landing_says_so_when_the_retention_rule_could_not_run_and_lands_anyway` — the landing says what it could not reclaim, and where the rest is reported:
- RED `a_recovery_enforces_the_rule_over_its_own_family_and_not_the_publications` — the recovery reclaimed the workspace the recovery before it left behind
- RED `a_landing_stops_the_daemon_the_landing_before_it_left_running` — the landing reclaimed the workspace before it

### `141-a-reclaimed-workspace-leaves-its-processes-running`

reclaiming a workspace stops asking which processes it left running, so the directory is unlinked while a daemon goes on holding everything that was in it.

- RED `reclaiming_a_workspace_stops_the_process_the_publication_left_running` — the rehearsal names the process the removal would reach for:
- RED `a_process_that_will_not_take_the_first_signal_is_ended_before_the_workspace_goes` — the report names the daemon it signalled — the shell holding the trap, and the sleep under it, are both working in there:
- RED `a_landing_stops_the_daemon_the_landing_before_it_left_running` — timed out after 60s waiting until the daemon the earlier landing left running has gone

### `142-a-process-that-ignores-the-first-signal-is-left-running`

the signal that ends a process which would not stop is the one it has already ignored, so a workspace whose daemon ignores SIGTERM is one nothing can ever reclaim.

- RED `a_process_that_will_not_take_the_first_signal_is_ended_before_the_workspace_goes` — a workspace whose daemon ignored the first signal is still reclaimed:

### `143-work-that-never-reached-the-origin-stops-keeping-a-workspace`

a run root stops being asked whether its clone holds work no origin has, so a publication that failed is reaped as if it had landed.

- RED `the_workspaces_holding_work_no_origin_has_are_bounded_and_the_oldest_beyond_it_goes` — <tmp>/.onevcs/workspaces/publications/feature-older-2d293-18ce5016151ad715-0 is one of the three newest workspaces holding unlanded work:
- RED `a_workspace_whose_merge_path_rejected_the_change_is_judged_and_keeps_the_work_it_never_landed` — a workspace holding work no origin has is kept:

### `144-the-failure-history-a-workspace-holds-is-never-bounded`

the bound on workspaces holding work no origin has stops being applied, so a scratch root keeps every failed publication for ever.

- RED `the_workspaces_holding_work_no_origin_has_are_bounded_and_the_oldest_beyond_it_goes` — the fourth-newest is beyond the bound and goes:
- RED `a_landing_applies_the_same_bound_to_the_workspaces_holding_work_no_origin_has` — the landing bounded the failure history it found

### `145-the-bound-keeps-the-oldest-failures-rather-than-the-newest`

the bound keeps the workspaces written longest ago rather than the most recent, so the failure somebody is asking about is the one that was reaped.

- RED `the_workspaces_holding_work_no_origin_has_are_bounded_and_the_oldest_beyond_it_goes` — <tmp>/.onevcs/workspaces/publications/feature-newest-30d78-18ce5018cf88cace-0 is one of the three newest workspaces holding unlanded work:

### `146-the-processes-this-one-descends-from-are-signalled-too`

the search for what a workspace left running stops leaving out this process and the ones it descends from, so a sweep run from inside a workspace stops the shell that ran it.

- RED `an_operator_sweeping_from_inside_a_workspace_is_not_stopped_by_their_own_sweep` — Unexpected failure.

### `147-content-the-base-already-carries-is-read-as-unpublished-work`

a run clone's branch is read as work that never reached the origin whenever a ref does not carry it, so every finished publication looks like a failure worth keeping.

- RED `a_workspace_whose_branch_the_base_already_carries_takes_no_place_in_the_bound` — a workspace whose branch the base already carries is spent, whatever its own commits are:

### `148-a-family-the-retention-pass-could-not-read-is-passed-over-in-silence`

the retention rule a landing enforces stops saying when it could not read the family at all, so a disk fills with nothing said anywhere.

- RED `a_landing_says_so_when_the_family_it_would_reclaim_cannot_be_listed` — the landing says which family it could not judge:

### `149-the-landing-a-merge-recorded-is-never-read`

the landing this host recorded for a branch is never read, so the most certain tier answers nothing and a lower one has to.

- RED `a_branch_this_host_landed_with_no_change_request_reads_as_landed_by_the_landing_it_recorded` — assertion `left == right` failed

### `14-an-identity-with-no-bar-is-only-diagnosed`

the refusal that nothing would be attested stops naming what answers it.

- RED `an_identity_with_no_bar_is_told_what_would_give_it_one` — Unexpected stderr, failed var.contains(executable pre-push hook)

### `150-the-change-requests-number-is-not-looked-for`

the change request's number is not looked for in the base's history, so a branch the host merged is judged by content alone.

- RED `a_branch_the_host_squash_merged_reads_as_landed_after_the_base_moves_over_its_own_paths` — assertion `left == right` failed

### `151-a-landing-leaves-no-record-on-the-base`

a publication that opens no change request leaves no landing trailer on the base, so nothing on the base says what it landed.

- RED `a_landing_this_host_kept_no_record_of_is_read_off_the_trailer_it_left_on_the_base` — assertion `left == right` failed: nothing recorded a landing for this name, and the base's own commit did: {"version":3,"ref":{"given":"pres

### `152-a-landing-nothing-records-is-read-as-unpublished`

the comparison of content answers no rather than unknown, so a landing nothing recorded reads as work nobody published.

- RED `a_branch_landed_with_no_change_request_and_not_through_this_host_reads_as_unknown` — assertion `left == right` failed: the base carries what it changed and nothing records that it landed: {"version":3,"ref":{"given":"feature/

### `153-a-landed-row-is-offered-to-be-resumed-anyway`

a row whose work reached the base carries the argv that publishes it again, and prints it as ready to resume.

- RED `a_landing_this_host_kept_no_record_of_is_read_off_the_trailer_it_left_on_the_base` — Unexpected stdout, failed var.contains(Nothing to resume)
- RED `a_branch_this_host_landed_with_no_change_request_reads_as_landed_by_the_landing_it_recorded` — assertion `left == right` failed: a landed row carries no argv, in the document as in the rendering: {"identity":"<tmp>/project","branch":{"

### `154-a-branch-that-landed-is-listed-as-work-to-publish`

recoverable stops withholding the branches whose work reached their base, so a landed one is a row like any other.

- RED `a_branch_the_host_squash_merged_reads_as_landed_after_the_base_moves_over_its_own_paths` — a branch whose change request merged is not offered to be published again: 1 preserved unpublished branch(es) across every registered identi

### `155-the-tiers-travel-under-names-nothing-teaches`

the tier that decided a landing travels under a spelling the surface record does not teach.

- RED `the_record_names_every_word_the_landing_answer_travels_as` — docs/inferred-surface.md teaches the landing vocabulary, and "recordedLanding" is a word the types spell that it does not name
- RED `a_branch_this_host_landed_with_no_change_request_reads_as_landed_by_the_landing_it_recorded` — assertion `left == right` failed

### `156-work-nobody-published-is-read-as-maybe-landed`

the comparison of content answers unknown whatever the base carries, so work nobody published reads as work that may have landed.

- RED `a_branch_nobody_published_reads_as_not_landed_and_keeps_the_command_that_lands_it` — assertion `left == right` failed

### `157-the-base-having-taken-the-same-change-is-no-obstacle`

a base whose history already took a change under the subject a landing of this branch would have carried is no obstacle to reporting the branch as work nobody published.

- RED `a_branch_landed_with_no_change_request_and_not_through_this_host_reads_as_unknown` — assertion `left == right` failed: the base took a change under the subject a landing of this branch would have carried, so whether it landed

### `158-only-the-number-a-host-parenthesises-is-looked-for`

only the number a host trails in parentheses is looked for in the base's history, so a change request a merge commit names in a sentence — or one a message quotes the URL of — is a landing nothing reads.

- RED `a_change_request_a_merge_commit_names_is_read_off_the_bases_history` — assertion `left == right` failed: the base's history names this change request, in the sentence a merge commit spells it in: {"version":3,"r
- RED `a_change_request_its_own_url_names_is_read_off_the_bases_history` — assertion `left == right` failed: the base's history quotes this change request's own URL: {"version":3,"ref":{"given":"feature/quoted-by-ur

### `159-a-landing-answers-for-work-it-never-carried`

a recorded landing stops being asked whether it carried everything the branch has changed since it forked, so a landing answers for work committed onto the branch after it.

- RED `a_landing_never_answers_for_work_the_branch_gained_after_it` — assertion `left != right` failed: a landing answers for the work it carried, and this branch has since gained work it did not: {"version":3,

### `15-a-recorded-base-is-refused-without-naming-it`

an unusable stack pointer is refused as a bare name rather than as the trailer it came from.

- RED `a_recorded_base_that_is_not_a_branch_names_the_trailer_that_says_so` — Unexpected stderr, failed var.contains(Onevcs-Change-Base:)

### `160-a-spent-copys-landing-answers-for-the-branch`

the copy that answers for a branch is whichever one shows a landing, so a run clone left at the commit that landed answers for a checkout holding commits nobody published.

- RED `a_landing_never_answers_for_work_the_branch_gained_after_it` — assertion `left != right` failed: a landing answers for the work it carried, and this branch has since gained work it did not: {"version":3,

### `160-push-evidence-is-conditional-again`

a publishing push records what it wrote only where it was refused, and never on the event, so what a green run's merge path said is thrown away.

- RED `a_push_a_hook_refuses_records_what_the_hook_wrote` — every publishing push records what it wrote
- RED `a_push_that_is_accepted_records_what_it_wrote_too` — an accepted push records what it wrote as well
- RED `a_pre_push_hook_that_rejects_the_push_is_reported_as_the_merge_path_refusing` — a stored log
- RED `a_refusing_merge_path_stops_the_publication_and_leaves_the_work_where_it_can_be_found` — a refused push stores what it wrote

### `161-a-conflict-names-nothing-it-conflicts-over`

a conflict stops carrying the paths git left unmerged, so every refusal about one says only that something conflicted.

- RED `a_conflict_across_more_files_than_a_refusal_can_name_says_how_many_it_left_out` — Unexpected stderr, failed var.contains(and 2 more)
- RED `a_conflict_whose_hunks_cannot_be_stored_is_still_reported_as_a_conflict` — Unexpected stderr, failed var.contains(shared \" 0.txt)
- RED `a_base_that_conflicts_with_the_branch_reports_its_own_exit_code` — Unexpected stderr, failed var.contains(in "shared.txt")
- RED `a_train_reports_each_way_a_candidate_can_fail_without_stopping` — Unexpected stdout, failed var.contains(claude/clashes-remote: skipped (conflict with the current base in "shared.txt"))

### `162-a-conflict-shows-none-of-its-hunks`

a conflict stops carrying the hunks git renders for it, so the evidence beside it is empty.

- RED `a_base_that_conflicts_with_the_branch_reports_its_own_exit_code` — Unexpected stdout, failed var.contains(from the session)

### `163-a-red-required-check-quotes-nothing`

a required check that concluded red is named and its log is not quoted, leaving the reason reachable only by fetching the artifact by hand.

- RED `a_red_required_check_ends_an_auto_merge_publication_and_quotes_the_log` — Unexpected stderr, failed var.contains(error: the regression is here)

### `164-the-bound-stops-without-saying-what`

the bound on watching the host stops without naming what was still unsettled, which is the silent failure it used to be.

- RED `a_change_the_host_never_lands_ends_at_the_bound_and_names_what_was_pending` — Unexpected stderr, failed var.contains(still unsettled: "gate")
- RED `a_branch_the_host_never_lands_is_bounded_and_says_what_was_pending` — Unexpected stderr, failed var.contains(still unsettled: "gate")
- RED `an_auto_merge_the_host_takes_and_never_performs_is_bounded_and_says_the_checks_were_fine` — Unexpected stderr, failed var.contains(every required check it declared had settled)
- RED `a_required_check_that_never_settles_is_bounded_rather_than_waited_on_forever` — Unexpected stderr, failed var.contains(still unsettled: "gate")
- RED `a_repository_that_declares_no_required_check_is_not_read_as_having_passed` — Unexpected stderr, failed var.contains(the host declared no required check on it at all)
- RED `a_change_request_the_host_reports_no_checks_on_is_bounded_rather_than_merged` — Unexpected stderr, failed var.contains(the host declared no required check on it at all)
- RED `landing_is_told_apart_from_a_queued_merge_and_from_a_change_that_closed` — Unexpected stderr, failed var.contains(still unsettled: "gate")

### `165-a-change-auto-publication-settles-at-queued`

a change-auto publication arms the host's merge and settles at queued, which is where it left a change blocked with nobody alive to report it.

- RED `a_change_the_host_holds_is_watched_until_it_lands_and_reports_the_commit` — Unexpected stdout, failed var.contains(merged at)
- RED `a_branch_the_host_holds_is_watched_until_it_lands` — Unexpected stdout, failed var.contains(merged at)
- RED `a_change_the_host_never_lands_ends_at_the_bound_and_names_what_was_pending` — Unexpected return code, failed var == 1
- RED `a_branch_the_host_never_lands_is_bounded_and_says_what_was_pending` — Unexpected return code, failed var == 1
- RED `a_red_required_check_ends_an_auto_merge_publication_and_quotes_the_log` — Unexpected return code, failed var == 1
- RED `an_auto_merge_the_host_takes_and_never_performs_is_bounded_and_says_the_checks_were_fine` — Unexpected return code, failed var == 1
- RED `a_merge_the_host_reports_is_recorded_on_the_branch_under_the_configured_prefix` — Unexpected stdout, failed var.contains(merged at)
- RED `a_landing_the_checkout_will_not_take_is_a_warning_rather_than_a_failed_publication` — Unexpected stdout, failed var.contains(merged at)
- RED `landing_is_told_apart_from_a_queued_merge_and_from_a_change_that_closed` — Unexpected stdout, failed var.contains(merged at)

### `166-checks-are-observed-for-nobody`

a change-direct publication stops consulting the host's checks before it asks for the merge, so a repository's required checks are observed for nobody.

- RED `a_host_that_will_not_describe_a_change_requests_checks_still_opens_one` — Unexpected return code, failed var == 2
- RED `a_host_that_queues_a_direct_merge_is_reported_as_queued_rather_than_as_landed` — assertion `left == right` failed: []

### `167-a-landing-is-never-recorded`

a merge the host reports is not recorded on the branch, so whether it landed can only be inferred from what the base happens to carry.

- RED `a_merge_the_host_reports_is_recorded_on_the_branch_under_the_configured_prefix` — git log --format=%B -1 feature/recorded failed in <tmp>/hosted:
- RED `a_landing_the_checkout_will_not_take_is_a_warning_rather_than_a_failed_publication` — Unexpected stderr, failed var.contains(the landing was not recorded)

### `168-a-landing-that-cannot-be-written-fails-the-publication`

a landing that could not be written down fails the publication, which reports a merge that has already happened as one that did not.

- RED `a_landing_the_checkout_will_not_take_is_a_warning_rather_than_a_failed_publication` — Unexpected failure.

### `169-the-amendment-stops-naming-a-failure`

the amendment stops naming one of the failures a publication can end with.

- RED `the_amendment_declares_every_failure_a_publication_can_end_with_and_its_exit_code` — assertion `left == right` failed: exactly one `rust` amendment must declare "ChecksFailed, ChecksUnsettled, PushRejected }"; found 0

### `16-the-trains-arguments-are-only-diagnosed`

integrate's argument refusals go back to stating what is wrong and nothing else.

- RED `a_train_offered_something_it_cannot_run_says_which_and_why` — Unexpected stderr, failed var.contains(`onevcs recoverable`)
- RED `a_train_asked_to_push_a_checkout_with_no_origin_says_what_to_run_instead` — Unexpected stderr, failed var.contains(re-run `onevcs integrate` without --push)

### `170-a-host-that-cannot-answer-says-not-yet`

a host that was never taught where a change landed answers "not yet" instead of refusing, so a publication watches to its bound and blames checks that were never the reason.

- RED `the_amendment_declares_the_question_a_watched_publication_asks_its_host` — a host that was never taught to answer must refuse rather than say `not yet`

### `171-the-amendment-stops-declaring-the-question-a-watch-asks`

the amendment declares the question a watching publication asks its host in a shape that cannot say "not yet".

- RED `the_amendment_declares_the_question_a_watched_publication_asks_its_host` — the amendment no longer declares the method a watch asks: pub trait RemoteHost {                       // the six above, unchanged, plus:

### `172-a-queued-direct-merge-is-reported-as-open`

a host that queues a direct merge is reported as having left the change open, which is a different thing from a merge it will perform later.

- RED `a_host_that_queues_a_direct_merge_is_reported_as_queued_rather_than_as_landed` — a queued merge is not a landing: Publication { session: SessionToken("<token>"), branch: "feature/queueing", policy: ChangeDirect, outcome: 

### `173-the-amendment-states-an-interval-nothing-polls-at`

the amendment states an interval this build does not poll at.

- RED `the_amendment_states_the_interval_this_build_asks_the_host_at` — the amendment no longer states the 30-second interval this build polls at

### `174-a-second-run-shares-the-checkout`

the lock is taken but never contended, so a second run under one checkout joins the first rather than being turned away.

- RED `a_second_run_under_one_checkout_is_refused_rather_than_joining_in` — the refusal owes the lock it met and what to do about it:

### `175-a-landing-record-is-read-as-describing-the-change`

a landing record is read as describing the change, so a branch that landed can be published under "chore: record the landing of ...".

- RED `a_branch_carrying_a_landing_record_is_never_published_under_it` — Unexpected success

### `176-a-lock-that-would-not-come-off-is-assumed-gone`

the harness assumes its lock came off, so a removal that failed leaves the lock behind saying nothing.

- RED `a_lock_the_run_cannot_put_down_is_said_rather_than_assumed` — stderr does not report "red-green: .logs/red-green.lock could not be removed":

### `177-a-failed-restore-keeps-the-lock`

the exit handler stops carrying the run's status by hand, so `set -e` ends it at a restore that failed and the lock stays held.

- RED `a_tree_the_run_cannot_put_back_is_the_one_failure_it_says_loudest` — the lock is released even when the tree could not be put back

### `17-a-branch-nobody-has-is-only-diagnosed`

the branch-keyed refusals stop naming the command that lists the branches there are.

- RED `a_branch_no_checkout_has_is_refused_by_the_command_that_lists_the_ones_that_do` — Unexpected stderr, failed var.contains(`onevcs recoverable`)
- RED `recovering_a_branch_no_checkout_has_names_everywhere_it_looked` — Unexpected stderr, failed var.contains(`onevcs recoverable`)
- RED `recovering_a_branch_with_nothing_ahead_of_its_base_says_there_is_nothing_to_recover` — Unexpected stderr, failed var.contains(`onevcs recoverable` lists the branches that do carry unpublished work)

### `18-a-skipped-candidate-is-handed-to-nobody`

the train's no-subject skip goes back to reporting the synthesis failure alone.

- RED `a_candidate_whose_content_the_base_already_carries_adds_no_second_commit` — Unexpected stdout, failed var.contains(publish it with `onevcs publish-branch claude/at-the-base --repo <tmp>/project --title <T>`)

### `197-one-pipe-is-drained-before-the-other-is-touched`

one pipe is read to EOF before the other is touched, so a merge path loud enough to fill the first one wedges the capture reading it.

- RED `a_merge_path_that_fills_its_pipes_and_passes_still_reaches_its_own_verdict` — the publication was killed at the journey's bound: a hook that writes past one pipe buffer wedged the capture reading it
- RED `a_merge_path_that_fills_its_pipes_and_refuses_still_reaches_its_own_verdict` — the publication was killed at the journey's bound: a hook that writes past one pipe buffer wedged the capture reading it

### `198-preserved-logs-are-never-pruned`

a branch's preserved logs are never pruned, so one re-pushing through a red merge path grows its directory without end.

- RED `a_branch_that_re_pushes_through_a_red_merge_path_cannot_grow_its_log_directory_forever` — assertion `left == right` failed: ["gate-0006.log", "gate-0007.log", "gate-0008.log", "gate-0012.log", "gate-0009.log", "gate-0004.log", "ga

### `199-evidence-leaves-the-library-unredacted`

evidence is stored as it arrived, so a credential a merge path echoed leaves the library inside an artifact.

- RED `a_merge_path_that_echoes_a_credential_records_only_that_it_had_one` — GITHUB_TOKEN=s3cret-value-nobody-should-see

### `19-the-verb-is-not-written-down`

the command surface record stops naming publish-branch, which is the drift the two readers exist to catch.

- RED `the_contract_and_clap_name_the_same_commands` — assertion `left == right` failed: the parser and the two documents that write the command surface down — docs/contract.md and docs/inferre

### `200-a-key-nobody-declared-is-read-in-silence`

a rule and a policy stop refusing a key nobody declared, so the gate no schema has any more is read back in silence.

- RED `a_rules_file_that_still_names_a_gate_is_not_a_shape_this_type_reads` — the approved fixture names a gate, and this type has no such field: RulesFile { version: 1, trailer_prefix: None, rules: [Rule { match: Rule
- RED `a_malformed_rules_file_is_rejected_at_the_boundary` — this must be rejected:

### `201-the-amendment-keeps-the-key-its-version-removed`

the amendment's version 3 fixture keeps the key that version removed, so the schema a consumer reads is one this build refuses.

- RED `the_version_3_fixture_round_trips_and_is_the_approved_one_without_its_gate` — the version 3 fixture must deserialize: Error("rules[0]: unknown field `gate`, expected one of `match`, `publication`, `approvals`", line: 7

### `202-a-spent-key-is-refused-rather-than-dropped`

a spent gate is left in the document rather than dropped, so a rules file at the version that still had one is refused rather than read.

- RED `a_rules_file_that_still_names_a_gate_is_read_at_the_versions_that_had_one_and_refused_at_three` — Unexpected failure.

### `203-a-spent-key-is-dropped-in-silence`

a spent gate is dropped in silence, so the only place an operator learns the key means nothing says nothing.

- RED `a_rules_file_that_still_names_a_gate_is_read_at_the_versions_that_had_one_and_refused_at_three` — assertion `left == right` failed: one deprecation line, naming the file: ""

### `204-content-the-base-carries-is-published-again`

a branch whose content the base already carries is taken as work to publish, both where a publication asks before it queues and in the squash that would build it.

- RED `a_branch_whose_content_already_landed_publishes_nothing_and_never_reaches_its_merge_path` — Unexpected failure.
- RED `a_branch_whose_content_already_landed_opens_no_change_request` — Unexpected stdout, failed var.contains(nothing to publish: the base already carries this branch's content)
- RED `an_answer_read_out_of_a_spent_copy_still_names_the_other_copies_of_the_name` — Unexpected stdout, failed var.contains(nothing to publish)

### `205-a-fired-bound-names-neither-itself-nor-its-knob`

a bound that fired says only that it did, naming neither the bound it was nor the knob that raises it.

- RED `a_wedged_pre_push_hook_is_stopped_by_the_bound_and_left_running_by_nothing` — Unexpected stderr, failed var.contains(bound 3s)

### `206-a-name-this-crate-never-wrote-is-read-as-an-attempt`

the one reader of a preserved attempt's name takes any number it can parse, so a file spelled the way this crate never writes one answers for a verdict and is spent by the retention bound.

- RED `a_workspace_whose_merge_path_recorded_no_verdict_is_retained_with_that_reason` — assertion `left == right` failed
- RED `a_branch_that_re_pushes_through_a_red_merge_path_cannot_grow_its_log_directory_forever` — assertion `left == right` failed: ["gate-0006.log", "gate-0007.log", "gate-0008.log", "gate-0012.log", "gate-0009.log", "gate-0004.log", "ga

### `207-retention-spends-a-file-this-crate-never-wrote`

retention takes every .log in the shared directory as an attempt of its own, so a file this crate never wrote counts against the bound and is deleted as the oldest of ours.

- RED `a_branch_that_re_pushes_through_a_red_merge_path_cannot_grow_its_log_directory_forever` — assertion `left == right` failed: ["gate-0006.log", "gate-0007.log", "gate-0008.log", "gate-0012.log", "gate-0009.log", "gate-notes.log", "g

### `208-a-train-nothing-will-judge-lands-in-silence`

a train whose identity has nothing on its merge path lands its candidates without saying so, so an operator learns what will never judge the work only after it is on the base.

- RED `a_train_whose_merge_path_runs_nothing_is_warned_about_before_it_lands_and_never_refused` — the train says what will not judge what it lands: ""

### `209-the-trains-push-verdict-keeps-no-evidence`

the train emits its own thinner push event instead of the one recorder every publishing push uses, so the run whose verification happens only at the push keeps no account of it.

- RED `a_train_whose_merge_path_runs_nothing_is_warned_about_before_it_lands_and_never_refused` — the push carries what the merge path wrote: {"v":1,"ts":"<time>","stream":"integrate-project","seq":8,"source":"vcs","kind":"push","labels":

### `20-a-round-header-is-not-checked`

the harness stops checking a mutation patch header before it runs the round.

- RED `a_round_with_no_one_subject_is_refused_before_any_of_them_runs` — expected a non-zero exit:
- RED `a_round_that_names_no_test_or_names_one_twice_is_refused` — expected a non-zero exit:

### `210-the-judged-tier-is-not-memoized`

the judged tier stops being cached, so every invocation is a fresh roll of a non-deterministic judge.

- RED `a_callers_judge_binary_does_not_change_the_verdict` — expected "replayed the recorded verdict for base" on stdout in exit status: 0
- RED `a_cleared_finding_replays_the_green_that_replaced_it` — expected "replayed the recorded verdict for base" in exit status: 0
- RED `a_forced_colour_environment_does_not_disguise_a_replay` — expected "replayed the recorded verdict for base" on stdout in exit status: 0
- RED `an_advanced_base_is_judged_again_and_then_replays_per_base` — expected "replayed the recorded verdict for base" in exit status: 0
- RED `an_ambient_global_cache_skip_is_reported_and_ignored` — assertion `left == right` failed
- RED `an_unchanged_tree_and_base_replays_the_recorded_verdict` — assertion `left == right` failed: the judge was rolled twice
- RED `skip_nx_cache_judges_again_without_replacing_the_recorded_verdict` — expected "replayed the recorded verdict for base" in exit status: 0

### `211-the-judge-configuration-leaves-the-cache-key`

the judge configuration fingerprint stops keying the cache, so a rule change nothing in the tree records replays the old verdict.

- RED `a_changed_judge_configuration_is_still_seen_through_a_callers_environment` — expected "judged this diff against base" on stdout in exit status: 0
- RED `a_changed_judge_configuration_outside_the_tree_is_judged_again` — expected "judged this diff against base" in exit status: 0
- RED `a_changed_llmlint_version_is_judged_again` — expected "judged this diff against base" in exit status: 0

### `212-the-base-commit-leaves-the-cache-key`

the resolved base commit stops keying the cache, so a verdict computed against one comparison answers for another.

- RED `an_advanced_base_is_judged_again_and_then_replays_per_base` — expected "judged this diff against base" in exit status: 0

### `213-the-cache-key-covers-one-file-instead-of-the-tree`

the cache key narrows from the whole workspace to one file, so a changed tree replays a verdict about the tree before it.

- RED `a_changed_tree_is_judged_again` — expected "judged this diff against base" in exit status: 0

### `214-an-ambient-global-cache-skip-is-honoured`

the judged tier stops ignoring an ambient global Nx cache skip, so an unrelated command's environment re-rolls the judge.

- RED `an_ambient_global_cache_skip_is_reported_and_ignored` — assertion `left == right` failed

### `215-a-callers-judge-binary-reaches-the-fingerprint`

the pinned runtime stops dropping the caller's LLMLINT_ONEHARNESS_BIN, which `llmlint config` renders — so one judged diff keys differently per caller.

- RED `a_callers_judge_binary_does_not_change_the_verdict` — expected "replayed the recorded verdict for base" on stdout in exit status: 0

### `216-a-fingerprint-that-cannot-be-produced-says-nothing`

the fingerprint stops naming a judge toolchain it cannot read, and reports a digest of nothing instead.

- RED `a_host_without_the_judge_is_told_which_command_installs_it` — expected "run 'just setup-llmlint'" on stderr in exit status: 1
- RED `the_fingerprint_names_an_unusable_judge_toolchain` — expected a failure, got exit status: 0

### `217-the-tier-judges-with-a-key-it-could-not-build`

the tier stops asking for the fingerprint before it judges, so a judge configuration nobody can read back silently leaves the cache key.

- RED `a_host_that_cannot_hash_the_judge_configuration_stops_the_tier` — expected a failure, got exit status: 0
- RED `a_host_without_the_judge_is_told_which_command_installs_it` — expected "the judge configuration could not be fingerprinted" on stderr in exit status: 1
- RED `a_judge_configuration_that_cannot_be_fingerprinted_stops_the_tier` — expected a failure, got exit status: 0
- RED `a_missing_pinned_runtime_helper_is_actionable` — expected "the judge configuration could not be fingerprinted" on stderr in exit status: 1

### `218-the-target-judges-whatever-base-it-is-handed`

the cached target stops checking that the base it was handed is a commit this checkout has.

- RED `the_target_run_through_just_nx_refuses_a_base_it_cannot_judge` — expected "must be a resolved commit id" in exit status: 1

### `219-a-judge-that-found-something-reports-success`

the judged tier stops carrying llmlint's exit status, so findings and a broken toolchain are recorded as a clean verdict.

- RED `a_judge_that_never_reached_a_verdict_is_never_replayed` — assertion `left == right` failed
- RED `findings_fail_the_tier_and_are_never_replayed` — assertion `left == right` failed: a red must be judged again

### `21-a-round-is-not-put-back`

the harness stops reverting a round once it is over.

- RED `a_round_is_recorded_and_the_tree_is_left_as_it_was_found` — Unexpected failure: subject.txt says mutated

### `220-an-unresolvable-base-reaches-the-judge`

the tier stops resolving the base ref to a commit before judging, so a ref naming nothing becomes a cache key nothing can invalidate.

- RED `an_unresolvable_base_is_refused_before_the_judge_runs` — expected "'no-such-ref' does not resolve to a commit" on stderr in exit status: 1

### `221-provenance-is-read-off-coloured-output`

the tier reads Nx's cache reporting without stripping colour, so a replayed verdict reports itself as freshly judged wherever something forces colour.

- RED `a_forced_colour_environment_does_not_disguise_a_replay` — expected "replayed the recorded verdict for base" on stdout in exit status: 0

### `222-the-verdict-is-handed-back-as-a-diagnostic`

the tier folds the judge's verdict into its diagnostics, so a caller reading the answer has to filter it out of the noise.

- RED `an_unchanged_tree_and_base_replays_the_recorded_verdict` — expected "31 rules: 31 passed, 0 failed" on stdout in exit status: 0

### `223-the-judges-own-view-of-the-run-is-dropped`

the tier drops what the judge said about the run rather than about the diff, so a finding arrives with no harness view behind it.

- RED `findings_fail_the_tier_and_are_never_replayed` — expected "fake-judge finding: tool_output_is_signal in scripts/llmlint-diff.sh" in exit status: 1

### `224-forwarded-arguments-are-not-checked`

the tier stops checking that what it forwards to Nx is an Nx option, so a caller's word reaches Nx's argument parser as whatever it reads it as.

- RED `an_argument_that_is_not_an_nx_option_is_refused_before_anything_is_judged` — expected a failure, got exit status: 0

### `225-a-judge-configuration-that-cannot-be-hashed-is-shrugged-off`

the fingerprint stops naming a host that cannot hash the judge configuration, and prints an empty digest instead.

- RED `a_host_that_cannot_hash_the_judge_configuration_stops_the_tier` — expected a failure, got exit status: 0

### `226-temporary-storage-that-cannot-be-opened-is-ignored`

the tier stops saying where to point TMPDIR when it cannot open storage for the judge's report.

- RED `a_tier_with_nowhere_to_write_its_report_says_where_to_point_it` — expected "could not open temporary storage for the judge report" on stderr in exit status: 1

### `227-the-pinned-runtime-helper-is-optional`

the fingerprint stops refusing when the pinned runtime environment cannot be loaded, and keys the cache under whatever the caller's environment was.

- RED `a_missing_pinned_runtime_helper_is_actionable` — expected "llmlint fingerprint: could not load the pinned runtime environment" on stderr in exit status: 1

### `228-the-verdict-is-never-recorded`

the judged verdict stops being recorded as a file, so a clean run has no count to report and a replay has nothing to restore.

- RED `a_cleared_finding_replays_the_green_that_replaced_it` — expected "31 rules: 31 passed, 0 failed" in exit status: 0
- RED `an_unchanged_tree_and_base_replays_the_recorded_verdict` — expected "31 rules: 31 passed, 0 failed" on stdout in exit status: 0

### `229-a-clean-run-prints-the-whole-run-instead-of-one-line`

a clean run stops being quiet, printing everything Nx and the judge said instead of the one line that carries the verdict.

- RED `an_unchanged_tree_and_base_replays_the_recorded_verdict` — assertion `left == right` failed: a clean run says one line: exit status: 0

### `22-a-dirty-tree-is-mutated-anyway`

the harness stops refusing a tree that carries uncommitted work.

- RED `a_tree_or_a_log_the_harness_cannot_safely_use_stops_it_before_any_round` — stderr does not report "the working tree has uncommitted changes":

### `230-a-failing-run-answers-on-the-success-stream`

a run with findings hands the judge's report back on stdout, where a caller reads this tier's answer, instead of as the diagnostic it is.

- RED `findings_fail_the_tier_and_are_never_replayed` — a run with findings answers nothing on stdout: exit status: 1

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

### `37-a-spent-name-answers-for-a-live-one`

the first repository to hold a name answers for it, spent or not.

- RED `a_name_used_a_second_time_continues_the_copy_that_spent_it_rather_than_forking_it` — No preserved unpublished branches. Every branch across the registered identities has reached its base or a remote.

### `38-a-spent-copy-of-a-name-is-published`

a branch is located by name alone, spent copy or not.

- RED `a_name_used_a_second_time_continues_the_copy_that_spent_it_rather_than_forking_it` — git show main:b.txt failed in <tmp>/project.git:

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
- RED `a_recovery_given_no_body_carries_its_attestation_on_the_branch_and_opens_with_none` — assertion `left == right` failed: a recovery composes no body either
- RED `a_complete_branch_of_a_team_identity_opens_the_change_request_its_rules_require` — assertion `left == right` failed: a branch-keyed publication composes no body

### `42-two-bodies-are-accepted`

naming the body twice stops being refused, and one of the two is silently taken.

- RED `naming_the_body_twice_is_refused_by_name_before_anything_is_published` — Unexpected return code, failed var == 2
- RED `naming_a_branchs_body_twice_is_refused_by_the_invocation_that_keeps_each_one` — Unexpected return code, failed var == 2
- RED `naming_a_recoverys_body_twice_is_refused_before_anything_is_attested` — Unexpected return code, failed var == 2

### `43-the-provider-drops-the-callers-body`

the testing repository stops handing the caller's body to the host it publishes through.

- RED `a_requested_title_and_body_are_the_ones_the_host_is_given` — no entry found for key

### `44-an-option-publish-takes-is-not-written-down`

the amendment stops naming one of the options `onevcs publish` takes.

- RED `the_amendment_names_every_option_publish_takes_that_the_approved_usage_does_not` — assertion `left == right` failed: the amendment and `onevcs publish` disagree about which options it takes beyond the approved two

### `45-a-recovery-attests-nothing`

recovery stops writing the attestation commit that carries the cleared marker forward.

- RED `a_recovery_given_no_body_carries_its_attestation_on_the_branch_and_opens_with_none` — the pushed branch must carry the recovery forward:

### `46-the-previous-version-is-refused-rather-than-read`

the readable range narrows back to one version, so a scenario written by the build before this one is refused.

- RED `a_document_at_the_previous_version_is_read_and_written_back_at_this_one` — the previous version reads: Invalid { reason: "the provider state at <tmp>/host.json: invalid input: the document declares version 4; this b

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
- RED `a_root_that_advances_before_the_queue_turn_is_resynced_without_the_stack_returning` — Unexpected failure.

### `54-a-stack-inferred-from-content`

a branch is stacked whenever its content looks stacked, rather than when its record says so.

- RED `a_branch_the_base_independently_matches_is_still_merged_because_no_record_stacks_it` — assertion `left == right` failed: the base arrived as the merge it always arrives as

### `55-an-unreadable-path-list-is-read-as-no-change`

a path list this process could not take whole is answered as a commit that changed nothing.

- RED `a_stack_whose_paths_this_process_cannot_read_is_answered_by_content_alone` — Unexpected failure.

### `56-a-stack-is-never-written-down`

the tip a session was cut from is never written down, so no session records a stack.

- RED `a_stacked_session_records_the_tip_it_was_cut_from_and_keeps_it_through_its_life` — assertion `left == right` failed: {"version":3,"token":"<token>","identity":"<tmp>/project","alias":"project","branch":"feature/above","base
- RED `a_continued_stacked_branch_records_where_its_own_work_begins_and_lands_only_that` — assertion `left == right` failed: the recorded tip is where the branch forked from the change below it, not its own tip: {"version":3,"token

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

### `63-a-moved-base-is-not-resynced`

a base that moved while this publication waited for its turn is never re-synced.

- RED `a_base_that_advances_conflictingly_while_a_publication_is_queued_is_reported_as_a_conflict` — Unexpected return code, failed var == 3

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

### `74-a-session-is-cut-from-a-stale-local-base`

the clone takes the lender's local branches for origin's own refs.

- RED `a_session_is_cut_from_origins_tip_rather_than_from_the_execution_checkouts_own_branch` — assertion `left == right` failed: the worktree is cut at what origin holds, not at what the lender remembers

### `75-an-ambiguous-reference-answers-with-the-first-candidate`

status answers about whichever piece of work a reference matched first, rather than refusing and naming them.

- RED `an_ambiguous_reference_is_refused_by_naming_the_candidates` — Unexpected return code, failed var == 2

### `75-a-pinned-resume-cuts-a-second-worktree`

a pinned branch a session already holds is not resumed.

- RED `a_pinned_branch_a_session_already_holds_resumes_it_rather_than_cutting_a_second_worktree` — assertion `left == right` failed: a pinned branch a session holds is that session

### `76-a-branch-a-session-still-holds-is-handed-to-the-verb-for-preserved-work`

status stops noticing that an open session holds the branch, so it names the verb that publishes work nobody holds.

- RED `work_a_run_left_in_its_own_clone_is_reported_with_the_verb_that_lands_it` — assertion `left == right` failed: an open session's branch is published through the session

### `76-an-occupied-session-is-taken-up-anyway`

the occupancy question is asked about a run root nobody is in.

- RED `a_pinned_branch_whose_session_is_occupied_opens_a_fresh_one_rather_than_refusing` — Unexpected failure.

### `77-a-base-that-carries-the-work-is-read-as-unpublished`

status stops reading the landing the base's own history records, and answers from the host and its own paperwork alone — which is how a change already squash-merged got reported as unpublished.

- RED `landing_is_told_apart_from_a_queued_merge_and_from_a_change_that_closed` — assertion `left == right` failed

### `77-an-unpinned-request-takes-up-somebody-elses-session`

a request that pinned no branch resumes whatever session it finds.

- RED `a_session_that_pins_no_branch_is_cut_fresh_every_time` — assertion `left != right` failed

### `78-a-host-that-could-not-be-asked-is-read-as-one-with-nothing-to-say`

status reports a host it could not reach as a host that answered there is nothing, so a gap in the report reads as an answer.

- RED `a_host_that_cannot_be_asked_leaves_its_section_unavailable_and_answers_the_rest` — assertion `left == right` failed

### `78-an-ambiguous-pin-picks-one-by-coin-toss`

two sessions holding one name, and whichever is found first is taken.

- RED `a_pinned_branch_whose_session_is_occupied_opens_a_fresh_one_rather_than_refusing` — assertion `left != right` failed: …nor the other

### `79-a-pin-resumes-a-session-nobody-asked-for`

a pin resumes a record without asking whether it is the session asked for, or still there.

- RED `a_pin_resumes_only_the_session_it_asked_for` — assertion `left != right` failed: a closed session is not resumed

### `79-a-source-nobody-named-is-not-searched-for`

import stops searching everywhere the identity keeps work when nobody passed --from, which is every branch a run left in its own clone.

- RED `a_branch_only_a_run_clone_has_is_imported_without_touching_any_working_tree` — Unexpected failure.
- RED `a_spent_name_does_not_block_an_import_under_another` — Unexpected failure.

### `80-a-name-the-destination-has-checked-out-is-written-anyway`

import stops refusing the one name a checkout has checked out, so a ref write leaves a working tree describing a commit the branch no longer names.

- RED `a_branch_only_a_run_clone_has_is_imported_without_touching_any_working_tree` — Unexpected return code, failed var == 2

### `80-a-stale-copy-of-a-branch-is-published`

the copies of a branch are ordered rather than compared, so the first checkout that holds work wins.

- RED `a_branch_two_checkouts_hold_is_published_from_the_copy_that_carries_the_other` — the copy that carries the other is the one that reached the base: README.md
- RED `copies_of_one_branch_that_have_diverged_refuse_the_landing_and_name_each_one` — Unexpected stderr, failed var.contains(no copy of it carries the rest)
- RED `a_replayed_copy_that_carries_none_of_the_one_it_replaced_is_refused_like_any_other` — Unexpected return code, failed var == 2
- RED `a_conflict_in_a_replayed_branchs_own_work_is_refused_with_the_replay_that_lands_it` — Unexpected return code, failed var == 2
- RED `recovering_a_branch_whose_copies_diverged_is_refused_by_the_verb_it_was_reached_by` — Unexpected stderr, failed var.contains(no copy of it carries the rest)

### `80-a-stated-subject-policy-is-never-asked`

the composed subject is never put to the repository, so a repository that states a policy has none applied.

- RED `a_repositorys_commit_msg_hook_refuses_the_subject_a_publication_would_land` — Unexpected return code, failed var == 1
- RED `a_commit_msg_hook_judges_the_explicit_title_a_publication_would_land_under` — Unexpected return code, failed var == 1
- RED `a_publication_the_repositorys_subject_policy_refuses_says_so_and_says_where_the_branch_went` — assertion `left == right` failed
- RED `a_commit_msg_hook_that_accepts_the_subject_leaves_the_publication_alone` — the hook recorded nothing at <tmp>/commit-msg-saw: No such file or directory (os error 2)
- RED `a_commit_msg_hook_that_cannot_run_refuses_the_publication_rather_than_passing_it` — Unexpected return code, failed var == 2
- RED `a_hook_that_refuses_without_a_word_is_still_reported_as_the_refusal_it_is` — Unexpected return code, failed var == 1
- RED `a_hook_that_never_answers_is_stopped_by_the_bound_and_left_running_by_nothing` — Unexpected return code, failed var == 2
- RED `a_locally_published_session_is_held_to_the_same_policy_as_a_branch` — Unexpected return code, failed var == 1

### `81-a-remote-ref-is-not-a-source`

import stops taking a remote ref as a source, leaving a branch only the origin has reachable by no verb.

- RED `a_branch_is_imported_from_another_checkout_and_from_a_remote_ref` — Unexpected failure.

### `81-a-repository-that-states-no-policy-is-given-one`

a repository with no executable commit-msg hook is given a policy anyway, rather than being left alone.

- RED `a_repository_with_no_commit_msg_hook_is_given_no_subject_policy` — Unexpected failure.
- RED `a_commit_msg_hook_git_itself_would_skip_is_skipped_here_too` — Unexpected failure.

### `81-a-tie-between-copies-is-broken-backwards`

equal tips are broken by the last checkout searched rather than by the search order.

- RED `copies_of_one_branch_at_one_commit_are_read_out_of_the_first_checkout_searched` — the first checkout searched is the one it was read out of:

### `82-a-choice-between-copies-is-made-silently`

the copy a landing chose is no longer said, so a stale selection and a current one read identically.

- RED `a_branch_two_checkouts_hold_is_published_from_the_copy_that_carries_the_other` — the copy that was published is named with the commit it held:
- RED `every_checkout_holding_the_branch_is_named_when_a_copy_is_chosen_between_them` — the copy that was published is named:
- RED `an_answer_read_out_of_a_spent_copy_still_names_the_other_copies_of_the_name` — the copy the answer came from is named:

### `82-an-alternate-name-is-dropped`

import writes the branch's own name whatever --as asked for, so work whose name is already spent has nowhere to go.

- RED `a_spent_name_does_not_block_an_import_under_another` — Unexpected stdout, failed var.contains(imported preserved/held)
- RED `a_branch_only_a_run_clone_has_is_imported_without_touching_any_working_tree` — Unexpected return code, failed var == 2

### `82-a-wordless-refusal-is-reported-as-nothing`

a hook that refuses without writing anything is reported as a refusal with nothing after it.

- RED `a_hook_that_refuses_without_a_word_is_still_reported_as_the_refusal_it_is` — Unexpected stderr, failed var.contains(The hook said:

### `83-a-choice-nobody-made-is-announced-anyway`

a copy is announced whether or not anything was chosen between, which is every landing after a session.

- RED `copies_of_one_branch_at_one_commit_are_read_out_of_the_first_checkout_searched` — and the copy it passed over is not the one an operator is sent to:
- RED `work_a_stopped_run_left_only_in_its_clone_is_reported_and_landed_by_the_command_named` — a lone copy is published without a word about copies

### `83-a-hooks-rewrite-is-taken-as-the-subject`

a hook that rewrites the message file has its rewrite taken as the subject, rather than only its verdict.

- RED `a_hook_that_rewrites_the_message_publishes_the_subject_it_was_asked_about` — assertion `left == right` failed

### `83-a-name-is-overwritten-with-whatever-arrives`

import stops refusing a non-fast-forward, so a name a checkout holds work under is written over by whatever the source has.

- RED `an_import_that_would_not_fast_forward_is_refused_naming_what_it_would_lose` — Unexpected return code, failed var == 2

### `84-a-missing-directory-and-a-missing-git-are-swapped`

the check on the working directory is inverted, so each way git cannot start is answered with the other's message.

- RED `a_git_command_whose_working_directory_is_gone_names_that_directory` — Unexpected stderr, failed var.contains(in <tmp>/project: that directory does not exist)
- RED `a_git_binary_nothing_can_find_still_names_the_binary` — Unexpected stderr, failed var.contains(is git installed and on PATH?)

### `84-another-programs-output-is-printed-as-it-arrived`

text a hook wrote is interpolated into the refusal exactly as it arrived, control characters and all.

- RED `a_repositorys_commit_msg_hook_refuses_the_subject_a_publication_would_land` — an escape sequence a hook wrote must not reach the terminal:

### `84-every-merge-path-verdict-is-read-as-a-pass`

status stops reading what a push event actually said, so a refusal and a word this build does not know both report as a pass.

- RED `the_last_merge_path_verdict_recorded_for_the_work_is_what_the_report_names` — assertion `left == right` failed

### `85-a-branch-a-stream-names-is-taken-on-trust`

status hands a stream's recorded branch to git without asking whether it is a branch name at all.

- RED `an_ambiguous_reference_is_refused_by_naming_the_candidates` — Unexpected stderr, failed var.contains(is a name git would not accept)

### `85-a-byte-that-is-not-text-takes-the-whole-output`

a hook whose refusal carries one undecodable byte has the whole refusal dropped rather than rendered around it.

- RED `a_hook_that_refuses_in_bytes_that_are_not_text_still_refuses` — the undecodable byte is shown as one and the rest of the refusal survives:

### `85-a-spent-copy-is-left-out-of-the-answer`

a copy whose content the base already carries is left out of the copies a landing reports, so the answer is not about every checkout holding the branch.

- RED `every_checkout_holding_the_branch_is_named_when_a_copy_is_chosen_between_them` — and so is the one the base already carries:
- RED `an_answer_read_out_of_a_spent_copy_still_names_the_other_copies_of_the_name` — and so is the other copy of the name:

### `86-a-hook-nobody-would-answer-for-is-read-as-absent`

a filesystem that will not say whether a hook is there is read as saying there is none.

- RED `a_hooks_directory_that_will_not_answer_is_refused_rather_than_read_as_empty` — Unexpected return code, failed var == 2

### `86-a-spent-copy-is-left-out-of-the-comparison`

the copies whose content the base already carries are left out of the comparison, so a lone work-carrying copy is chosen beside a tip nothing descends from.

- RED `a_copy_the_base_already_carries_is_compared_like_any_other_and_refuses_a_landing` — Unexpected return code, failed var == 2

### `86-the-report-does-not-say-which-shape-it-is`

the status report stops declaring its schema version, so a consumer reads a shape it inferred from the keys it could find.

- RED `the_status_report_is_the_versioned_object_its_goldens_record` — assertion `left == right` failed: the report declares the version the surface record documents
- RED `both_checked_in_goldens_read_back_as_reports_and_write_themselves_again` — assertion `left == right` failed: the full golden and the report it reads back as disagree

### `87-a-field-that-holds-nothing-is-written-as-null`

the status report writes an optional field even when it holds nothing, so a consumer that never heard of a session is handed one that is null.

- RED `the_status_report_is_the_versioned_object_its_goldens_record` — assertion `left == right` failed: the object a report with nothing optional in it writes is its checked-in golden; re-make crates/onevcs/tes

### `88-a-version-this-build-cannot-read-is-read-anyway`

the report's schema version stops being checked where the object is read, so a document written to a shape this build does not have is read as one it does.

- RED `a_report_declaring_a_version_this_build_does_not_read_is_refused_where_it_is_read` — a version this build does not read is refused: Report { version: ReportVersion(0), reference: Reference { given: "feature/full", kind: Branc

### `89-a-field-a-report-omits-cannot-be-read-back`

a field the report leaves out when it holds nothing becomes one a reader requires, so the very bytes this build writes are bytes it cannot read.

- RED `both_checked_in_goldens_read_back_as_reports_and_write_themselves_again` — the full golden reads back as a report: missing field `notes` at line 75 column 1

### `90-a-commit-a-checkout-cannot-see-is-read-as-a-failure`

the copies whose commit a checkout cannot see are asked about anyway, so an absence git cannot answer ends the landing instead of losing the comparison.

- RED `a_copy_whose_checkout_cannot_see_the_others_commit_loses_the_comparison` — Unexpected failure.
- RED `a_branch_two_checkouts_hold_is_published_from_the_copy_that_carries_the_other` — Unexpected failure.
- RED `copies_of_one_branch_that_have_diverged_refuse_the_landing_and_name_each_one` — Unexpected stderr, failed var.contains(no copy of it carries the rest)
- RED `a_copy_the_base_already_carries_is_compared_like_any_other_and_refuses_a_landing` — Unexpected stderr, failed var.contains(no copy of it carries the rest)
- RED `recovering_a_branch_whose_copies_diverged_is_refused_by_the_verb_it_was_reached_by` — Unexpected stderr, failed var.contains(no copy of it carries the rest)
- RED `an_import_with_no_source_refuses_diverged_copies_and_sends_the_operator_back_to_it` — Unexpected stderr, failed var.contains(no copy of it carries the rest)
- RED `a_name_used_a_second_time_continues_the_copy_that_spent_it_rather_than_forking_it` — Unexpected failure.

### `91-a-character-that-renders-as-nothing-is-passed-through`

only the control codes are escaped, so a character that reorders or hides what a hook wrote is passed to the terminal as it arrived.

- RED `a_hook_that_refuses_in_characters_that_render_as_nothing_is_read_as_it_was_written` — a character that renders as nothing must not reach the terminal: '\u{202e}'

### `92-a-bound-no-duration-can-hold-is-accepted`

a bound is checked for being finite and above zero but for neither of the two things that make it one anything can wait on, so an oversized value reaches the conversion that panics on it.

- RED `an_unusable_bound_is_refused_rather_than_silently_reverting_to_unbounded` — Unexpected return code, failed var == 2

### `93-a-bound-that-stops-at-the-pipes`

the exit of a run whose pipes have both reached EOF is collected by an unbounded wait, so a hook that closes its streams and keeps running outlives the bound.

- RED `a_hook_that_closes_both_streams_and_keeps_running_is_still_stopped_by_the_bound` — Unexpected return code, failed var == 2

### `94-a-bound-no-instant-can-be-advanced-by-is-accepted`

a bound is checked for being one a duration can hold but never for being one an instant can be advanced by, so an oversized value reaches the deadline arithmetic that panics on it.

- RED `an_unusable_bound_is_refused_rather_than_silently_reverting_to_unbounded` — Unexpected return code, failed var == 2

### `95-a-body-refusal-names-a-command-nobody-ran`

the refusal for two bodies names `onevcs publish` whichever verb the body was given to.

- RED `naming_the_body_twice_is_refused_by_name_before_anything_is_published` — Unexpected stderr, failed var.contains(onevcs publish <token> --body-file <tmp>/drafted-body.md)
- RED `naming_a_branchs_body_twice_is_refused_by_the_invocation_that_keeps_each_one` — Unexpected stderr, failed var.contains(onevcs publish-branch feature/two-bodies --repo <tmp>/hosted --body-file <tmp>/drafted-body.md)
- RED `naming_a_recoverys_body_twice_is_refused_before_anything_is_attested` — Unexpected stderr, failed var.contains(onevcs recover feature/interrupted --repo <tmp>/two-bodies --body-file <tmp>/drafted-body.md)

### `96-a-branch-keyed-body-is-dropped`

the branch-keyed verbs stop carrying the caller's body into the publication.

- RED `a_complete_branch_opens_its_change_request_with_the_body_the_caller_drafted` — assertion `left == right` failed
- RED `a_recovery_opens_its_change_request_with_the_body_the_caller_drafted` — assertion `left == right` failed

### `97-the-record-stops-naming-the-sentence-it-departs-from`

the record stops quoting the approved sentence the branch-keyed body options depart from.

- RED `the_record_names_the_body_sentence_the_branch_keyed_verbs_depart_from` — docs/inferred-surface.md must quote the sentence the branch-keyed body options depart from, so a reader of either document finds the other

### `98-a-pinned-branch-is-cut-fresh-over-the-work`

a pinned branch goes back to being cut fresh from the base whatever already carries it.

- RED `a_branch_pin_naming_work_that_already_exists_continues_it_rather_than_cutting_fresh` — the work is in the worktree: Os { code: 2, kind: NotFound, message: "No such file or directory" }
- RED `a_continued_branch_publishes_the_commits_its_base_does_not_carry` — the session continues the branch's own work
- RED `a_name_used_a_second_time_continues_the_copy_that_spent_it_rather_than_forking_it` — the second use opens on the branch the first one left, with the base it has since landed on merged in

### `99-a-continued-session-never-merges-its-base`

a continued session stops merging the base it publishes into.

- RED `a_continued_branch_publishes_the_commits_its_base_does_not_carry` — and the base it publishes into is merged in
- RED `a_continued_branch_whose_base_conflicts_is_refused_naming_where_it_is_and_what_lands_it` — Unexpected return code, failed var == 3

