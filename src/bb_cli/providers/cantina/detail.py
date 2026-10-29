"""Readable detail pages for Cantina bounties and competitions, in the shared document style."""
from __future__ import annotations

import re

from bb_cli.evm.render import Doc, Style, datetime_utc, date, joined, relative, table
from .discovery import status as lifecycle

SEVERITY_ORDER = ['critical', 'high', 'medium', 'low', 'informational', 'gas']
_LINK_RE = re.compile(r'https?://[^\s<>\)\]"\']+')


def _amount(value, currency) -> str:
    try:
        number = float(value)
    except (TypeError, ValueError):
        return '-'
    if number <= 0:
        return '-'
    if currency in (None, '', 'USD', 'USDC', 'USDT', 'DAI'):  # dollar-pegged: show as dollars
        return f"${number:,.0f}" + (f" {currency}" if currency and currency != 'USD' else '')
    return f"{number:,.0f} {currency}"


def _is_competition(record: dict) -> bool:
    return 'contest' in (record.get('kind') or '')


def _groups(record: dict) -> list[dict]:
    return [g for g in record.get('assetGroups') or [] if not g.get('hidden')]


def _rewards(group: dict, currency) -> str:
    rows = sorted(group.get('rewards') or [], key=lambda r: SEVERITY_ORDER.index(r.get('severity')) if r.get('severity') in SEVERITY_ORDER else 99)
    parts = [f"{r['severity'].capitalize()} {_amount(r.get('maxReward'), currency)}" for r in rows if _amount(r.get('maxReward'), currency) != '-']
    return ' · '.join(parts)


def _repository(record: dict) -> str:
    url = record.get('gitRepoUrl') or ''
    commit = record.get('commitHash') or ''
    return f"{url} @ {commit[:10]}" if url and commit else url


def _header(doc: Doc, record: dict) -> None:
    doc.raw(doc.style.bold(record.get('name') or record.get('id', '')) + '  ' + doc.style.link(record.get('url') or ''))


def overview(doc: Doc, record: dict) -> None:
    s = doc.style
    _header(doc, record)
    currency = record.get('currencyCode')
    timeframe = record.get('timeframe') or {}
    company = record.get('company') or {}
    pairs = [('Status', s.status(lifecycle(record).upper()))]
    if _is_competition(record):
        pairs += [
            ('Reward pool', _amount(record.get('totalRewardPot'), currency)),
            ('Starts', f"{datetime_utc(timeframe.get('start'))}  {s.dim(relative(timeframe.get('start')))}"),
            ('Ends', f"{datetime_utc(timeframe.get('end'))}  {s.dim(relative(timeframe.get('end')))}"),
        ]
    else:
        pairs += [('Max reward', _amount(record.get('totalRewardPot'), currency)), ('Launched', date(timeframe.get('start')))]
    fee = record.get('submissionFee')
    pairs += [
        ('KYC', 'required' if record.get('kycRequired') else 'not required' if record.get('kycRequired') is False else ''),
        ('Submission fee', _amount(fee, currency) if fee and float(fee) > 0 else ''),
        ('Triage', 'platform managed' if record.get('cantinaTriaged') else ''),
        ('Severities', joined(record.get('consideredSeverities'))),
        ('Findings', f"{record['totalFindings']:,} submitted" if isinstance(record.get('totalFindings'), int) else ''),
        ('Company', company.get('name') or ''),
        ('Website', company.get('website') or ''),
        ('Repository', _repository(record)),
    ]
    doc.fields(pairs)


def information(doc: Doc, record: dict) -> None:
    doc.section('Information')
    rewards = [(g.get('name') or '-', _rewards(g, record.get('currencyCode'))) for g in _groups(record) if not g.get('outOfScope')]
    rewards = [(name, line) for name, line in rewards if line]
    doc.fields([
        ('Allowed severities', joined(record.get('allowedSeverities'))),
        ('Rewarded severities', joined(record.get('consideredSeverities'))),
    ])
    if rewards:
        doc.sub('Rewards by asset group')
        doc.fields(rewards)
    if record.get('instructions'):
        doc.sub('Instructions')
        doc.text(record['instructions'])


def _asset_rows(group: dict) -> list[list[str]]:
    rows = []
    for asset in group.get('assets') or []:
        if asset.get('hidden'):
            continue
        reference = (asset.get('reference') or '').strip()
        description = (asset.get('description') or '').strip()
        if not reference and _LINK_RE.fullmatch(description):
            reference, description = description, ''
        lines = [l.strip().lstrip('-* ').replace('`', '') for l in description.splitlines() if l.strip() not in ('', '-')]
        rows.append([asset.get('name') or '-', reference or '-', ' '.join(lines)])
    return rows


def scope(doc: Doc, record: dict) -> None:
    doc.section('Scope')
    s = doc.style
    groups = _groups(record)
    if not groups:
        doc.fields([('Repository', _repository(record)), ('Readme', record.get('readmePath') or '')])
        doc.raw(s.dim('  No asset list published; the repository README describes the scope.'))
        return
    for group in [g for g in groups if not g.get('outOfScope')]:
        rows = _asset_rows(group)
        doc.sub(f"{group.get('name') or 'Assets'} ({len(rows)})")
        if line := _rewards(group, record.get('currencyCode')):
            doc.raw('  ' + s.dim(line))
        if rows:
            doc.raw('\n'.join('  ' + r for r in table(['NAME', 'TARGET', 'NOTES'], rows, s, flex=2).splitlines()))
    excluded = [g for g in groups if g.get('outOfScope')]
    if excluded:
        doc.sub('Out of scope')
        for group in excluded:
            if group.get('name', '').strip():
                doc.raw('  ' + s.bold(group['name']))
            doc.text(group.get('description'))
            rows = _asset_rows(group)
            if rows:
                doc.raw('\n'.join('  ' + r for r in table(['NAME', 'TARGET', 'NOTES'], rows, s, flex=2).splitlines()))


def resources(doc: Doc, record: dict) -> None:
    doc.section('Resources')
    company = record.get('company') or {}
    doc.fields([
        ('Repository', record.get('gitRepoUrl') or ''),
        ('Commit', record.get('commitHash') or ''),
        ('Readme', record.get('readmePath') or ''),
        ('Website', company.get('website') or ''),
        ('GitHub', f"https://github.com/{company['github']}" if company.get('github') else ''),
        ('Twitter', f"https://x.com/{company['twitter']}" if company.get('twitter') else ''),
    ])
    links = list(dict.fromkeys(_LINK_RE.findall(record.get('instructions') or '')))
    for group in _groups(record):
        links += [l for l in _LINK_RE.findall(group.get('description') or '') if l not in links]
    if links:
        doc.sub(f'Links ({len(links)})')
        doc.raw('\n'.join('  ' + doc.style.link(l) for l in links))


def _team(entry: dict) -> str:
    team = entry.get('team') or {}
    names = [m.get('username') or m.get('name') for m in team.get('members') or []]
    return team.get('name') or team.get('handle') or ', '.join(n for n in names if n) or '-'


def leaderboard(doc: Doc, record: dict) -> None:
    doc.section('Leaderboard')
    board = record.get('leaderboard') or {}
    if not _is_competition(record):
        doc.raw(doc.style.dim('  Bounties have no leaderboard.'))
        return
    if not board.get('available'):
        doc.raw(doc.style.dim('  ' + (board.get('reason') or 'Not published yet.')))
        return
    ranking = (board.get('data') or {}).get('ranking') or []
    currency = record.get('currencyCode')
    severities = ('critical', 'high', 'medium', 'low')
    # Finished competitions often publish rewards with zeroed finding counts; omit empty columns.
    counted = [sev for sev in severities if any(e.get(f'{sev}Findings') for e in ranking)]
    rows = [[str(e.get('position', '-')), _team(e), _amount(e.get('reward'), currency),
             *(str(e.get(f'{sev}Findings') or 0) for sev in counted)] for e in ranking]
    headers = ['#', 'TEAM', 'REWARD', *(sev.upper()[:4] for sev in counted)]
    doc.raw('\n'.join('  ' + line for line in table(headers, rows, doc.style, flex=1).splitlines()))


SECTIONS = {'overview': overview, 'information': information, 'scope': scope, 'resources': resources, 'leaderboard': leaderboard}


def detail(record: dict, sections: list[str], style: Style) -> str:
    doc = Doc(style)
    names = list(SECTIONS) if 'all' in sections else sections
    if 'all' in sections and not _is_competition(record):
        names.remove('leaderboard')
    if 'overview' not in names:
        _header(doc, record)
    for name in names:
        SECTIONS[name](doc, record)
    return doc.render()
