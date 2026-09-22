"""Focused non-display tests for the Tk control-surface interaction helpers."""
import unittest
from types import SimpleNamespace
from unittest import mock
from typing import cast

from emu.gui import CanvasKey, encoder_drag_steps, trigger_position


class EncoderDragStepsTest(unittest.TestCase):
    def test_drag_emits_relative_detents_in_both_directions(self):
        self.assertEqual(encoder_drag_steps(23), 2)
        self.assertEqual(encoder_drag_steps(-23), -2)

    def test_sub_detent_drag_does_not_emit_an_encoder_event(self):
        self.assertEqual(encoder_drag_steps(7), 0)
        self.assertEqual(encoder_drag_steps(-7), 0)

    def test_custom_threshold_is_supported_for_gesture_callers(self):
        self.assertEqual(encoder_drag_steps(15, threshold=5), 3)


class TriggerGridTest(unittest.TestCase):
    def test_sixteen_trigger_codes_fill_two_rows_of_eight_in_order(self):
        self.assertEqual(
            [trigger_position(index) for index in range(16)],
            [(row, column) for row in range(2) for column in range(8)],
        )


class CanvasKeyEventTest(unittest.TestCase):
    def test_press_and_release_invoke_momentary_callbacks_in_order(self):
        events = []
        draw = mock.Mock()
        key = cast(CanvasKey, SimpleNamespace(
            pressed=False,
            _draw=draw,
            on_press=lambda: events.append('press'),
            on_release=lambda: events.append('release'),
            command=None,
        ))

        CanvasKey._press(key, None)
        CanvasKey._release(key, None)

        self.assertEqual(events, ['press', 'release'])
        self.assertFalse(key.pressed)
        self.assertEqual(draw.call_count, 2)

    def test_command_runs_after_a_completed_click(self):
        events = []
        key = cast(CanvasKey, SimpleNamespace(
            pressed=True,
            _draw=mock.Mock(),
            on_press=None,
            on_release=lambda: events.append('release'),
            command=lambda: events.append('command'),
        ))

        CanvasKey._release(key, None)

        self.assertEqual(events, ['release', 'command'])
