"""Owner-only addressing and one-shot local pairing (Hermes-inspired)."""
import re
import secrets
import time


def incoming(author, owner, bot, dm, text, bot_id):
    if not owner or author != owner or bot:
        return None
    mention = re.compile(r'<@!?' + re.escape(bot_id) + '>')
    if not dm and not mention.search(text):
        return None
    return mention.sub('', text).strip() or None


class Pairing:
    def __init__(self, now=None):
        self.code = secrets.token_urlsafe(18)
        self.expires = (time.monotonic() if now is None else now) + 300
        self.used = False

    def accept(self, text, now=None):
        now = time.monotonic() if now is None else now
        if self.used or now >= self.expires or not secrets.compare_digest(text, self.code):
            return False
        self.used = True
        return True
