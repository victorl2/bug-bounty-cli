"""Readable detail pages for Sherlock bug bounties and contests, in the shared document style."""
from __future__ import annotations

from bb_cli.evm.render import Doc, Style, date, datetime_utc, joined, money, relative
from ..common import iso, links, rows_table


def _amount(value, currency) -> str:
    try:
        number = float(value)
    except (TypeError, ValueError):
        return ''
    return money(number) + (f' {currency}' if currency and number > 0 else '')


def _contest(record: dict) -> bool:
    return record.get('kind') == 'competition'


def _repos(record: dict) -> list[dict]:
    """Repositories in scope with their commit and file count, from either record shape."""
    if _contest(record):
        return [{'repo': s.get('repo'), 'commit': s.get('commit_hash'), 'branch': s.get('branch_name'),
                 'files': s.get('files') or [], 'nsloc': s.get('total_nsloc')} for s in record.get('scope') or []]
    repos = {}
    for s in record.get('covered_scope') or []:
        if s.get('repo_name') and s.get('selected', True):
            repo = repos.setdefault((s['repo_name'], s.get('commit_hash')), {'repo': s['repo_name'], 'commit': s.get('commit_hash'),
                                                                              'branch': s.get('branch_name'), 'files': [], 'nsloc': None})
            repo['files'].append({'name': s.get('file_path')})
    return list(repos.values())


def _repo_url(repo: dict) -> str:
    base = f"https://github.com/{repo['repo']}"
    return f"{base}/tree/{repo['commit']}" if repo.get('commit') else base


def _contracts(record: dict) -> list[dict]:
    return [s for s in record.get('covered_scope') or [] if s.get('address') and s.get('selected', True)]


def _header(doc: Doc, record: dict) -> None:
    doc.raw(doc.style.bold(record.get('title') or str(record.get('id'))) + '  ' + doc.style.link(record.get('url') or ''))


def overview(doc: Doc, record: dict) -> None:
    from .api import status
    s = doc.style
    _header(doc, record)
    kind = record.get('kind')
    pairs = [('Status', s.status(status(kind, record).upper()))]
    if _contest(record):
        token = record.get('token')
        start, end = iso(record.get('starts_at')), iso(record.get('ends_at'))
        pairs += [
            ('Type', record.get('type_label') or ''),
            ('Reward pool', _amount(record.get('rewards'), token)),
            ('Auditor pool', _amount(record.get('prize_pool'), token)),
            ('Judging pool', _amount(record.get('judging_prize_pool'), token) if record.get('judging_prize_pool') else ''),
            ('Starts', f'{datetime_utc(start)}  {s.dim(relative(start))}'),
            ('Ends', f'{datetime_utc(end)}  {s.dim(relative(end))}'),
            ('KYC', 'required' if record.get('requires_kyc') else 'not required' if record.get('requires_kyc') is False else ''),
            ('Languages', joined(record.get('languages'))),
            ('nSLOC', f"{record['nsloc']:,}" if record.get('nsloc') else ''),
            ('Issues', f"{record['num_competition_issues']:,} submitted" if record.get('num_competition_issues') else ''),
            ('Lead judge', record.get('lead_judge_handle') or ''),
            ('Lead auditor', record.get('lead_senior_auditor_handle') or ''),
            ('Repository', f"https://github.com/{record['template_repo_name']}" if record.get('template_repo_name') else ''),
        ]
    else:
        currency = record.get('display_currency')
        deposits = ' · '.join(f"{d.get('severity_name')} {_amount(d.get('deposit_amount'), currency)}"
                              for d in sorted(record.get('severities') or [], key=lambda d: d.get('display_order') or 0))
        pairs += [
            ('Max payout', _amount(record.get('payout'), currency)),
            ('Live since', date(iso(record.get('live_since')))),
            ('Updated', date(iso(record.get('last_updated')))),
            ('Categories', joined(c.get('name') for c in record.get('categories') or [])),
            ('Submission deposit', deposits),
            ('Contracts', str(len(_contracts(record))) if _contracts(record) else ''),
            ('Repositories', joined(r['repo'] for r in _repos(record))),
        ]
    doc.fields(pairs)


def information(doc: Doc, record: dict) -> None:
    doc.section('Information')
    if record.get('short_description'):
        doc.text(record['short_description'])
    tiers = [(str(t.get('severity', {}).get('name') or '').strip(), f"{t.get('unlock_percentage', 0):.0f}% of the pool unlocks at {t.get('min_count')} valid")
             for t in record.get('rewards_tiers') or []]
    if tiers:
        doc.sub('Reward unlocks')
        doc.fields(tiers)
    if record.get('description'):
        doc.sub('Details')
        doc.text(record['description'])
    if record.get('submission_severity_notice'):
        doc.sub('Submission notice')
        doc.text(record['submission_severity_notice'])


def scope(doc: Doc, record: dict) -> None:
    doc.section('Scope')
    contracts, repos = _contracts(record), _repos(record)
    if contracts:
        doc.sub(f'Contracts ({len(contracts)})')
        rows_table(doc, ['NAME', 'NETWORK', 'ADDRESS'], [[c.get('display_name') or '-', (c.get('network') or '-').title(), c['address']] for c in contracts], 0)
    for repo in repos:
        doc.sub(f"{repo['repo']} ({len(repo['files'])} files" + (f", {repo['nsloc']:,} nSLOC)" if repo.get('nsloc') else ')'))
        doc.raw('  ' + doc.style.link(_repo_url(repo)))
        rows_table(doc, ['FILE', 'NSLOC'], [[f.get('name') or '-', str(f.get('nsloc') or '-')] for f in repo['files']], 0)
    extra = [s for s in (record.get('url_scope') or []) + (record.get('extra_scope') or []) if s]
    if extra:
        doc.sub('Other assets')
        doc.raw('\n'.join('  ' + (e if isinstance(e, str) else e.get('url') or e.get('name') or str(e)) for e in extra))
    if not (contracts or repos or extra):
        doc.raw(doc.style.dim('  No asset list published; the information section describes the scope.'))


def resources(doc: Doc, record: dict) -> None:
    doc.section('Resources')
    doc.fields([
        ('Repository', f"https://github.com/{record['template_repo_name']}" if record.get('template_repo_name') else ''),
        ('Judging', f"https://github.com/{record['judging_repo_name']}" if record.get('judging_repo_name') else ''),
    ])
    found = [_repo_url(r) for r in _repos(record)] + links(record.get('description'), record.get('short_description'))
    found = list(dict.fromkeys(found))
    if found:
        doc.sub(f'Links ({len(found)})')
        doc.raw('\n'.join('  ' + doc.style.link(l) for l in found))


def leaderboard(doc: Doc, record: dict) -> None:
    doc.section('Leaderboard')
    doc.raw(doc.style.dim('  Not published through the public API; see the page on the site.'))


SECTIONS = {'overview': overview, 'information': information, 'scope': scope, 'resources': resources, 'leaderboard': leaderboard}


def render(record: dict, sections: list[str], style: Style) -> str:
    doc = Doc(style)
    names = [n for n in SECTIONS if n != 'leaderboard'] if 'all' in sections else sections
    if 'overview' not in names:
        _header(doc, record)
    for name in names:
        SECTIONS[name](doc, record)
    return doc.render()
