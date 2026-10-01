#!/usr/bin/env python3
"""Witness state-I/O admission must honor the producer's current halt first."""
import unittest
from observation_speed import state_call


class Replies:
    def __init__(self, replies):
        self.replies = iter(replies)
        self.calls = []

    def call_result(self, name, arguments):
        self.calls.append((name, arguments))
        return next(self.replies)

    def call(self, name, arguments):
        self.calls.append((name, arguments))
        return {'status': 'completed'}


class Admission(unittest.TestCase):
    def test_supported_boundary_is_not_advanced(self):
        for name in ('save_state', 'load_state'):
            w = Replies([({'status': 'completed'}, False)])
            result, error = state_call(w, {}, name, {'path': 'fixture.state'})
            self.assertIsNone(error)
            self.assertEqual(result['status'], 'completed')
            self.assertEqual(w.calls, [(name, {'path': 'fixture.state'})])

    def test_only_unsafe_halt_seeks_an_instruction_boundary(self):
        rejected = {'error': {'code': 'unsafe_halt'}}
        w = Replies([(rejected, True), ({'status': 'completed'}, False)])
        self.assertIsNone(state_call(w, {}, 'save_state', {})[1])
        self.assertEqual(w.calls, [('save_state', {}),
                                  ('step', {'unit': 'instructions', 'count': 1}),
                                  ('save_state', {})])

    def test_other_errors_and_unsupported_instruction_steps_do_not_advance(self):
        for error, status in [
            ({'error': {'code': 'io_error'}}, {}),
            ({'error': {'code': 'unsafe_halt'}},
             {'contracts': {'constraints': {'execution.step.units': ['frames']}}}),
        ]:
            w = Replies([(error, True)])
            self.assertEqual(state_call(w, status, 'save_state', {}), (None, error))
            self.assertEqual(w.calls, [('save_state', {})])


if __name__ == '__main__':
    unittest.main()
