import datetime

from cleo.io.io import IO
from poetry.plugins.plugin import Plugin
from poetry.poetry import Poetry


class VersionPlugin(Plugin):
    def activate(self, poetry: Poetry, io: IO):
        io.write_line("Setting project's version...")
        poetry.package.version = (
            f"{datetime.datetime.today().strftime('%Y.%m.%d.%H.%M')}"
        )
