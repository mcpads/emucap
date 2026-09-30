#!/usr/bin/env python3
"""Opt-in adapter-independent witness for exact input under agent pacing.

From one origin, the same frozen `tap` runs at several paces. A pace changes only host time between
guest frames, so every run must end with the same guest memory as the 100 percent reference, and
that memory must differ from the same frames advanced without input. Memory that differs between two
identical 100 percent runs is host-dependent (for example a host renderer's frame buffer copy); it
is excluded from both claims and reported. The origin is a saved state, or a fresh launch where the
host cannot save while frozen. The profile supplies launch arguments, warmup frames and optionally
the buttons to tap; every check uses the public Control MCP surface and live capabilities.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import sys

sys.path.insert(0, str(Path(__file__).resolve().parent))
from observation_speed import ROOT, Witness, free_port, warmup  # noqa: E402

PREFERRED_BUTTONS = ('start', 'run', 'a', 'enter', 'return', 'space', 'button1', 'cross')
PACES = [{'mode': 'limited', 'percent': 50}, {'mode': 'limited', 'percent': 400}, {'mode': 'unlimited'}]


def button_names(status):
    listed = status.get('input_buttons') or {}
    if isinstance(listed, dict):
        listed = listed.get('buttons') or []
    return [entry if isinstance(entry, str) else entry.get('name') for entry in listed]


def choose_buttons(profile, status):
    if 'tap_buttons' in profile:
        return profile['tap_buttons']
    names = button_names(status)
    for name in PREFERRED_BUTTONS:
        if name in names:
            return [name]
    raise AssertionError(f'no preferred button among {names}; set tap_buttons in the profile')


def ram_windows(capability):
    # Writable guest memory shows an input's effect; ROM, BIOS and whole address spaces do not.
    chosen = []
    for window in capability['windows']:
        name = window['memory_type'].lower()
        if 'rom' in name or 'bios' in name:
            continue
        if 'ram' in name or name in ('main', 'ee', 'physical'):
            chosen.append(window)
    return chosen[:6] or capability['windows'][:1]


COVERAGE_BYTES = 4 * 1024 * 1024


def window_batches(capability, window):
    # Read a window whole up to COVERAGE_BYTES, otherwise that many bytes spread evenly; each batch
    # stays within the advertised bounds.
    ranges_per_batch = min(capability['max_ranges'], 64)
    chunk = min(capability['max_range_bytes'], capability['max_total_bytes'] // ranges_per_batch)
    covered = min(window['length'], COVERAGE_BYTES)
    count = max(1, covered // chunk)
    chunk = min(chunk, window['length'] // count)
    stride = window['length'] // count
    ranges = [{'memory_type': window['memory_type'], 'address': window['address'] + i * stride,
               'length': chunk} for i in range(count)]
    return [ranges[i:i + ranges_per_batch] for i in range(0, len(ranges), ranges_per_batch)]


def readable(w, batches):
    # An advertised window can depend on live mode (e.g. a plane only visible in one display mode).
    kept = []
    for ranges in batches:
        reply = w.process.request('tools/call', {'name': 'read_memory_batch', 'arguments': {'ranges': ranges}},
                                  timeout=300)
        if not reply.get('result', {}).get('isError'):
            kept.append(ranges)
    return kept


def digest(w, batches):
    # One hash per batch locates a divergence; their hash is the run's digest.
    # Reads bypass the request log, which would otherwise keep every megabyte of hex it hashes.
    parts = []
    for ranges in batches:
        reply = w.process.request('tools/call', {'name': 'read_memory_batch', 'arguments': {'ranges': ranges}},
                                  timeout=300)
        assert not reply.get('result', {}).get('isError'), reply
        hasher = hashlib.sha256()
        for read in reply['result']['structuredContent']['reads']:
            hasher.update(read['hex'].encode())
        parts.append(hasher.hexdigest()[:16])
    return hashlib.sha256(''.join(parts).encode()).hexdigest(), parts


def instruction_units(status):
    units = status.get('contracts', {}).get('constraints', {}).get('execution.step.units', ['instructions'])
    return 'instructions' in units


def state_call(w, status, name, arguments):
    # Some hosts save and load only at a proven main-CPU instruction halt. A CPU idling at a frame
    # boundary reaches one only after it wakes, so later attempts step further before retrying.
    content = None
    for count in (1, 1, 5, 23, 101, 499, 2003):
        if instruction_units(status):
            w.call('step', {'unit': 'instructions', 'count': count})
        reply = w.process.request('tools/call', {'name': name, 'arguments': arguments}, timeout=300)
        content = reply.get('result', {}).get('structuredContent', reply)
        if not reply.get('result', {}).get('isError'):
            return content, None
        if 'unsafe_halt' not in json.dumps(content):
            return None, content
    return None, content


class Session:
    """One managed launch; a relaunch starts a new generation from power-on."""

    def __init__(self, w, arguments):
        self.w, self.arguments, self.launch = w, arguments, None

    def start(self):
        self.launch = self.w.call('launch', self.arguments)
        # A new generation carries its own capability revision.
        self.w.revision = None
        status = self.w.call('status')
        if status['state'] != 'frozen':
            self.w.call('pause')
        return status

    def stop(self):
        if self.launch:
            stopped = self.w.call('stop', {'launch_id': self.launch['launch_id']})
            self.launch = None
            return stopped
        return None


class Origin:
    """A saved state, or a fresh launch plus fixed warmup where the host cannot save when frozen."""

    def __init__(self, w, out, status, warmup_frames, session):
        self.w, self.status, self.warmup_frames, self.session = w, status, warmup_frames, session
        if w.call('status')['state'] != 'frozen':
            w.call('pause')
        save = {'path': str(out / 'origin.state')}
        if 'instruction_snapshot_capture' in json.dumps(status):
            save['snapshot_key'] = 'input-pacing-origin'
        saved, self.save_error = state_call(w, status, 'save_state', save)
        self.path = save['path'] if saved is not None else None

    def kind(self):
        return 'saved_state' if self.path else 'relaunch_warmup'

    def restore(self):
        if self.path:
            loaded, error = state_call(self.w, self.status, 'load_state', {'path': self.path})
            assert loaded is not None, error
            return
        # A soft reset can keep RAM and media state, so only power-on repeats the origin.
        self.session.stop()
        self.status = self.session.start()
        self.w.speed({'mode': 'unlimited'})
        warmup(self.w, self.status, self.warmup_frames)


def run(w, origin, batches, policy, buttons, press, after):
    # A tap releases on its own edge frame between the press and the trailing frames.
    frames = press + 1 + after
    origin.restore()
    w.speed(policy)
    start = w.call('status')['frame']
    if buttons:
        reply = w.call('tap', {'buttons': buttons, 'press_frames': press, 'after_frames': after})
        assert reply.get('state') == 'frozen' and 'status' not in reply, reply
    else:
        reply = w.call('step', {'unit': 'frames', 'count': frames})
        assert reply['status'] == 'completed', reply
    seconds = w.seconds()
    # status.frame may count a different unit than frame steps (e.g. VI fields), so it is compared
    # across runs rather than against the requested count.
    end = w.call('status')['frame']
    total, parts = digest(w, batches)
    return {'policy': policy, 'input': bool(buttons), 'steps': frames, 'frames': end - start,
            'seconds': seconds, 'digest': total, 'batch_digests': parts}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--output', type=Path, required=True)
    parser.add_argument('--profile', type=Path, required=True,
                        help='JSON: launch_plan, launch overrides, env, warmup_frames, tap_buttons, '
                             'press_frames, after_frames')
    args = parser.parse_args()
    out = args.output.resolve()
    out.mkdir(parents=True, exist_ok=False)
    profile = json.loads(args.profile.read_text())
    env = dict(os.environ, EMUCAP_REPO_ROOT=str(ROOT), EMUCAP_PORT=str(free_port()),
               EMUCAP_EMU_HOME=str(out / 'home'))
    env.update(profile.get('env', {}))
    w = Witness(out, env)
    session = None
    try:
        w.process.initialize()
        w.call('bootstrap')
        plan = w.call('launch_plan', profile['launch_plan'])
        if not plan['ready_to_launch'] and plan.get('next_action', {}).get('kind') == 'review_input':
            (out / 'reviewed_media.json').write_text(json.dumps(plan['next_action']['review'], indent=2))
            plan = w.call('launch_plan', plan['next_action']['then_call']['arguments'])
        assert plan['ready_to_launch'], plan
        session = Session(w, {**plan['preferred_launcher']['args'], **profile.get('launch', {})})
        status = session.start()
        assert status['contracts']['state'] == 'validated', status
        (out / 'identity.json').write_text(json.dumps({'launch': session.launch, 'status': status}, indent=2))
        w.speed({'mode': 'unlimited'})
        warmup(w, status, profile.get('warmup_frames', 1))
        buttons = choose_buttons(profile, status)
        press, after = profile.get('press_frames', 2), profile.get('after_frames', 30)
        capability = status['memory_batch_capability']
        ranges = [batch for window in ram_windows(capability) for batch in window_batches(capability, window)]
        result = {'buttons': buttons, 'press_frames': press, 'after_frames': after,
                  'hashed_windows': sorted({batch[0]['memory_type'] for batch in ranges})}
        origin = Origin(w, out, status, profile.get('warmup_frames', 1), session)
        ranges = readable(w, ranges)
        result['hashed_windows'] = sorted({batch[0]['memory_type'] for batch in ranges})
        result['hashed_bytes'] = sum(r['length'] for batch in ranges for r in batch)
        result['origin'] = origin.kind()
        if origin.save_error:
            result['save_error'] = origin.save_error
        normal = {'mode': 'limited', 'percent': 100}

        def baseline():
            # Batches that differ between two identical runs hold host-dependent memory (e.g. a
            # host renderer's frame buffer copy or a host-seeded clock); claims exclude them.
            reference = run(w, origin, ranges, normal, buttons, press, after)
            repeat = run(w, origin, ranges, normal, buttons, press, after)
            control = run(w, origin, ranges, normal, None, press, after)
            mask = [i for i, (a, b) in enumerate(zip(reference['batch_digests'], repeat['batch_digests']))
                    if a == b]
            effect = any(reference['batch_digests'][i] != control['batch_digests'][i] for i in mask)
            return reference, repeat, control, mask, effect

        reference, repeat, control, mask, effect = baseline()
        # Intros and boot logos ignore input; move a saved origin forward until the tap matters.
        seek = 0
        while origin.path and not effect and seek < profile.get('seek_attempts', 8):
            seek += 1
            origin.restore()
            w.speed({'mode': 'unlimited'})
            warmup(w, status, profile.get('seek_frames', 300))
            origin = Origin(w, out, status, profile.get('warmup_frames', 1), session)
            reference, repeat, control, mask, effect = baseline()
        result['seek_frames'] = seek * profile.get('seek_frames', 300)
        paced = [run(w, origin, ranges, policy, buttons, press, after)
                 for policy in profile.get('paces', PACES)]
        # A short tap at the slowest pace keeps the wall time bounded.
        short = run(w, origin, ranges, normal, buttons, 1, 2)
        slowest = run(w, origin, ranges, {'mode': 'limited', 'percent': 1}, buttons, 1, 2)

        def same(a, b):
            return a['frames'] == b['frames'] and all(
                a['batch_digests'][i] == b['batch_digests'][i] for i in mask)

        result.update({
            'runs': [reference, repeat, control, *paced, short, slowest],
            'repeatable_batches': len(mask),
            'unrepeatable_batches': [
                {'memory_type': ranges[i][0]['memory_type'], 'address': ranges[i][0]['address']}
                for i in range(len(ranges)) if i not in mask],
            'input_effect': effect,
            'pace_equivalent': all(same(r, reference) for r in paced) and same(slowest, short),
        })
        result['passed'] = bool(mask) and effect and result['pace_equivalent']
        (out / 'result.json').write_text(json.dumps(result, indent=2))
        print(json.dumps(result, indent=2))
    finally:
        try:
            stopped = session.stop() if session else None
            if stopped:
                (out / 'stop.json').write_text(json.dumps(stopped, indent=2))
        finally:
            w.process.close()


if __name__ == '__main__':
    main()
