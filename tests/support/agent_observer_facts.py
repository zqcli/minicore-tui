"""Bind actual installed terminal facts to the real Agent completion owner."""

from canonical_observer import ObservationError
from agent_observer_checks import target


def bind_completion_facts(rows, installs, views):
    facts = {}
    for row in rows:
        if row['event'] != 'completion_registered':
            continue
        key = target(row)
        if key in facts:
            raise ObservationError('duplicate actual Agent completion identity')
        facts[key] = row
    for key, view in views.items():
        for terminal in (entry for entry in view['inventory'] if entry['kind'] == 'turn_end'):
            fact = facts.get((*key, terminal['loop_id']))
            if fact is None or terminal['outcome'] != (fact.get('outcome_kind') or 'unknown'):
                raise ObservationError('actual UI terminal changed original Agent outcome classification')
            saving = ('saved' if fact['persistence'] == 'persisted' else 'save unconfirmed'
                      if fact['persistence_uncertain'] or fact.get('completion_kind') == 'internal_error'
                      else 'not saved' if fact['persistence'] == 'failed' else None)
            if terminal['saving'] != saving:
                raise ObservationError('actual UI Saving differs from original Agent completion')
    for installed in installs:
        fact = facts.get(target(installed))
        if fact is None or (installed['completion_seq'], installed['input_id']) != (fact['seq'], fact['input_id']):
            raise ObservationError('installed canonical identity differs from actual Agent completion')
    return facts
