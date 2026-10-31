"""Synthetic fault lifecycle regressions; not actual injected-chain evidence."""

import copy
import unittest

from agent_observer_checks import bind_queries, validate_faults
from canonical_observer import ObservationError


class FaultLifecycleTests(unittest.TestCase):
    def fixture(self):
        common = dict(session_id='s', load='l', loop_id='a', session=1, turn=1)
        step = dict(id='page-a', session=1, turn=1, at='result_page', action='pause')
        fault = dict(common, id='page-a', point='result_page', action='pause')
        rows = [dict(common, event='loop_installed', event_seq=1),
                dict(fault, event='fault_ready', reason='paused', event_seq=2),
                dict(fault, event='fault_applied', reason='paused', event_seq=3),
                dict(fault, event='fault_released', reason='release', event_seq=4)]
        return rows, dict(schema=1, steps=[step])

    def test_actual_plan_pause_requires_all_matching_targeted_transitions(self):
        rows, plan = self.fixture()
        self.assertEqual(len(validate_faults(rows, plan)), 3)
        mutations = [lambda rows: rows.pop(1), lambda rows: rows.pop(),
                     lambda rows: rows[2].update(reason='unknown'),
                     lambda rows: rows[-1].update(load='other-load'),
                     lambda rows: rows[-1].update(reason='timeout'),
                     lambda rows: rows[2].update(action='rollback'),
                     lambda rows: rows[2].update(turn=2),
                     lambda rows: rows[2].update(event_seq=1)]
        for mutate in mutations:
            with self.subTest(mutate=mutate):
                values = copy.deepcopy(rows)
                mutate(values)
                with self.assertRaises(ObservationError):
                    validate_faults(values, plan)

    def test_valid_plan_cannot_be_claimed_from_an_empty_trace(self):
        rows, plan = self.fixture()
        with self.assertRaises(ObservationError):
            validate_faults(rows[:1], plan)
        with self.assertRaises(ObservationError):
            validate_faults(rows, None)

    def test_incomplete_ui_targets_cannot_borrow_another_targets_unavailable_reply(self):
        row = dict(query_id=1, session_id='s', load='l', loop_id='a', cursor=None, next_cursor=None,
                   requested_result_id=None, result_id=None, total=0, availability='unavailable',
                   unavailable_reason='expired', error=None)
        bind_queries([row], [], {('s', 'l', 'a'): 'unavailable'})
        with self.assertRaisesRegex(ObservationError, 'own matching query outcome'):
            bind_queries([row], [], {('s', 'l', 'a'): 'unavailable', ('s', 'l', 'b'): 'unavailable'})
        row['unavailable_reason'] = None
        with self.assertRaises(ObservationError):
            bind_queries([row], [], {('s', 'l', 'a'): 'unavailable'})

    def test_unknown_append_and_terminal_drop_have_exact_outcomes(self):
        common = dict(session_id='s', load='l', loop_id='a', session=1, turn=1)
        for action, point, reason in (('unknown', 'append', 'append_unknown'),
                                      ('drop_terminal', 'runtime_events', 'hold_until_runtime_join')):
            with self.subTest(action=action):
                step = dict(id='fault-a', session=1, turn=1, at=point, action=action)
                rows = [dict(common, event='loop_installed', event_seq=1),
                        dict(common, event='fault_applied', event_seq=2, id='fault-a', point=point, action=action, reason=reason)]
                if action == 'drop_terminal':
                    rows.append(dict(common, event='runtime_tail_dropped', event_seq=3, id='fault-a', buffered_events_discarded=2))
                validate_faults(rows, dict(steps=[step]))
                rows[1]['reason'] = 'unknown-success'
                with self.assertRaises(ObservationError):
                    validate_faults(rows, dict(steps=[step]))


if __name__ == '__main__':
    unittest.main(verbosity=2)
