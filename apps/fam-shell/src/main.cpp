#include <QApplication>
#include <QCommandLineParser>
#include <QElapsedTimer>
#include <QFile>
#include <QFileInfo>
#include <QTextStream>
#include <QDir>
#include <QPixmap>
#include <QTimer>

#include "famcore.h"
#include "iconcache.h"
#include "mainwindow.h"
#include "theme.h"

#include <algorithm>

int main(int argc, char *argv[]) {
    /* Everything below is measured from process entry, so a slow start can be told apart from a
     * slow view. */
    QElapsedTimer boot;
    boot.start();
    /* The row layout is the contract, so a version mismatch is not a warning: a shell that read v1
     * JSON out of what is now a fixed-length buffer would paint garbage and call it a folder. The
     * check runs before QApplication because stderr is the only thing worth writing with yet. */
    if (fam_abi_version() != FAM_ABI_VERSION) {
        fprintf(stderr,
                "famcore.dll exposes ABI version %u; this shell was built for %u and will not run "
                "against anything else\n",
                fam_abi_version(), FAM_ABI_VERSION);
        return 75;
    }

    QApplication app(argc, argv);
    QApplication::setApplicationName(QStringLiteral("FileAutoManage"));
    QApplication::setApplicationVersion(QStringLiteral("0.1.0"));
    QApplication::setOrganizationName(QStringLiteral("FileAutoManage"));

    QCommandLineParser parser;
    parser.setApplicationDescription(
        QStringLiteral("FAM resource manager shell over the Rust core"));
    parser.addHelpOption();
    parser.addVersionOption();
    const QCommandLineOption configOption(
        QStringLiteral("config"), QStringLiteral("Read <path> instead of the platform config."),
        QStringLiteral("path"));
    const QCommandLineOption queryOption(
        QStringLiteral("query"), QStringLiteral("Run this search once the window is shown."),
        QStringLiteral("terms"));
    const QCommandLineOption watchOption(
        QStringLiteral("watch"),
        QStringLiteral("Open <path> and keep it current once shown."),
        QStringLiteral("path"));
    const QCommandLineOption grabOption(
        QStringLiteral("self-check"),
        QStringLiteral("Render the window, save a screenshot to <path> and exit."),
        QStringLiteral("path"));
    const QCommandLineOption mutateOption(
        QStringLiteral("self-check-mutate"),
        QStringLiteral("With --watch and --self-check: write files into the watched folder, move one "
                       "of them into another folder and copy it back, and remove them again - so a "
                       "re-read and a transfer both happen while a view is open."));
    const QCommandLineOption deepOption(
        QStringLiteral("self-check-deep"),
        QStringLiteral("With --watch and --self-check: descend <count> folders, then click a column "
                       "header. Past the chain's depth cap this is what drops the oldest column."),
        QStringLiteral("count"));
    const QCommandLineOption themeOption(
        QStringLiteral("theme"),
        QStringLiteral("light, dark or system (the default)."), QStringLiteral("scheme"));
    parser.addOption(configOption);
    parser.addOption(queryOption);
    parser.addOption(watchOption);
    parser.addOption(grabOption);
    parser.addOption(mutateOption);
    parser.addOption(deepOption);
    parser.addOption(themeOption);
    parser.process(app);

    /* A windowed executable has no console, so the headless gate would otherwise prove nothing but
     * pixels: everything the shell does goes to a log beside the screenshot. Installed before the
     * window exists, because start-up is exactly what the gate needs timed. */
    static QFile log(parser.value(grabOption) + QStringLiteral(".log"));
    static QTextStream out(&log);
    if (parser.isSet(grabOption) && log.open(QIODevice::WriteOnly | QIODevice::Truncate)) {
        qInstallMessageHandler([](QtMsgType type, const QMessageLogContext &,
                                  const QString &message) {
            const char *level = "info";
            switch (type) {
            case QtWarningMsg: level = "warning"; break;
            case QtCriticalMsg: level = "error"; break;
            default: break;
            }
            out << level << ": " << message << Qt::endl;
        });
    }

    qInfo("qt is up %lldms after process entry", boot.elapsed());

    /* The design system goes on the application, not on the window: a style sheet applied after the
     * first widget exists leaves every size hint computed against whatever was there before, and the
     * row delegate reads its colours at paint time from the same singleton, so "before the window" is
     * what keeps the sheet and the hand-painted row agreeing.
     *
     * Fusion rather than the native Windows style, for the same reason the design sample gives: this
     * look is entirely the sheet's, and a style that draws its own gradients and focus glows wherever
     * the sheet is silent will disagree with the token set. It is a real change of rendering path for
     * the shell, which is why the headless gates run again after it. */
    QApplication::setStyle(QStringLiteral("Fusion"));
    Scheme scheme = Theme::systemScheme();
    const QString requested = parser.value(themeOption).toLower();
    if (requested == QLatin1String("light")) {
        scheme = Scheme::Light;
    } else if (requested == QLatin1String("dark")) {
        scheme = Scheme::Dark;
    } else if (!requested.isEmpty() && requested != QLatin1String("system")) {
        qWarning("unknown --theme \"%s\"; using the desktop's, which is %s",
                 qUtf8Printable(requested), scheme == Scheme::Dark ? "dark" : "light");
    }
    Theme::instance().setScheme(scheme);
    app.setFont(Theme::instance().applicationFont());
    app.setStyleSheet(Theme::instance().styleSheet());
    /* The readability claim belongs in the log, not only in a note: `contrastReport()` recomputes the
     * WCAG ratios of the pairs this theme actually paints, so a run records the numbers rather than
     * repeating them. */
    qInfo("theme: %s\n%s", qUtf8Printable(Theme::instance().name()),
          qUtf8Printable(Theme::instance().contrastReport()));

    MainWindow window(parser.value(configOption), parser.value(queryOption));
    window.setWindowTitle(QStringLiteral("FAM — resource manager"));
    window.resize(960, 620);
    window.show();
    qInfo("window shown %lldms after process entry", boot.elapsed());

    if (parser.isSet(grabOption)) {
        const QString target = parser.value(grabOption);
        /* A mutation needs a window wide enough for the core to notice the write time, re-read the
         * directory and let the shell's 250ms poll see the resync, so the grab moves out with it. */
        const bool mutate = parser.isSet(mutateOption);
        /* Scroll first, grab a second later: the scroll is what makes every paint ask the icon
         * worker for its extension, and the second is what lets the answers land, so the counters
         * below are read after the code path that could leak an HICON has actually run. */
        QTimer::singleShot(1500, [&window]() { window.scrollForSelfCheck(); });
        if (mutate) {
            /* One timeline that walks the whole browse surface: a file appears in the open folder,
             * the chain descends into a subfolder, a search scoped to *that* folder runs, the box is
             * cleared to go back, and only then is the file removed - so the deletion lands on a
             * shallower column while a deeper one is on screen. Every one of those is a path a run
             * with no hands on the keyboard would otherwise never take. */
            QTimer::singleShot(2000, [&window]() { window.createSelfCheckProbe(); });
            QTimer::singleShot(2500, [&window]() { window.descendForSelfCheck(); });
            QTimer::singleShot(3300,
                               [&window]() { window.runSelfCheckSearch(QStringLiteral("zip")); });
            QTimer::singleShot(4300, [&window]() { window.clearSelfCheckSearch(); });
            QTimer::singleShot(4700, [&window]() { window.sortDeepestForSelfCheck(); });
            QTimer::singleShot(5000, [&window]() { window.goUpForSelfCheck(); });
            /* The tags, on the file this run already owns: reading them, adding one, taking one off,
             * and trying to take off nothing. Before this step the shell could only ever add, so
             * nothing here would have had a way to fail. */
            QTimer::singleShot(5200, [&window]() { window.tagsForSelfCheck(); });
            QTimer::singleShot(5300, [&window, target]() {
                /* A sibling of the main shot rather than a suffix on it: `x.png.tags.png` is the
                 * kind of name that makes someone delete the file unread. */
                const QFileInfo shot(target);
                window.grabTagDialogForSelfCheck(shot.dir().filePath(
                    shot.completeBaseName() + QStringLiteral(".tags.png")));
            });
            /* The row's own half of the same tags: the beat reads them into the role a delegate paints
             * from, and the second beat on an untouched window must not ask again. A claim no other
             * step can make, because every other step reads the model rather than the beat.
             * Before the probe is removed, and after the go-up at 5000 put the file's own folder back
             * on screen as the deepest column. */
            QTimer::singleShot(5350, [&window]() { window.rowTagsForSelfCheck(); });
            QTimer::singleShot(5420, [&window, target]() {
                const QFileInfo shot(target);
                window.grabRowTagsForSelfCheck(shot.dir().filePath(
                    shot.completeBaseName() + QStringLiteral(".rowtags.png")));
            });
            /* The vocabulary strip's round trip: a chip is clicked, the query it writes runs and
             * settles with the tagged file in it, and a second click clears. It runs after the row
             * steps' tags are off, so the one chip the strip shows is this step's own. */
            QTimer::singleShot(5700, [&window]() { window.tagSearchForSelfCheck(); });
            QTimer::singleShot(5960, [&window, target]() {
                const QFileInfo shot(target);
                window.grabTagFilterForSelfCheck(shot.dir().filePath(
                    shot.completeBaseName() + QStringLiteral(".filter.png")));
            });
            /* The chip on the row itself, clicked like a hand clicks it - after the filter strip's
             * steps and before the drive pick replaces the chain they were pointed at. */
            QTimer::singleShot(6040, [&window]() { window.rowChipClickForSelfCheck(); });
            /* The note the agent could always write and the person could never see: a round trip
             * through the dialog's own operations, then the dialog itself as a picture. */
            QTimer::singleShot(6350, [&window]() { window.notesForSelfCheck(); });
            QTimer::singleShot(6550, [&window, target]() {
                const QFileInfo shot(target);
                window.grabNoteDialogForSelfCheck(shot.dir().filePath(
                    shot.completeBaseName() + QStringLiteral(".note.png")));
            });
            /* The probe file used to be removed here, 80ms after the row-tag shot started pumping the
             * event loop - and a shot that pumps the loop can let a later timer run before it finishes,
             * which took the row out from under the frame it was supposed to be proving. The removal at
             * the end of the run is the one that clears every file this run wrote, so the file lives
             * until then and the shot has room to breathe. */
            /* Last, because it replaces the chain: a drive pick is the sidebar doing its only job,
             * and the frame after it shows what the reader would see. */
            QTimer::singleShot(6700, [&window]() { window.pickFirstDriveForSelfCheck(); });
            /* Then the split, because the frame it produces is the one worth looking at: two panes,
             * each on its own folder, each with its own chain. Collapsing it again is the same button
             * doing the other half of its job, and the collapsed frame is the one a person who never
             * wanted a second column gets. */
            QTimer::singleShot(7200, [&window]() { window.toggleSplitForSelfCheck(true); });
            QTimer::singleShot(7700, [&window]() { window.toggleSplitForSelfCheck(false); });
            /* And open it once more before the frame is taken: the collapse is the half of the
             * button the log proves, the expanded frame is the half a person will actually look at,
             * and one timeline can carry both without inventing a second screenshot path. */
            QTimer::singleShot(8100, [&window]() { window.toggleSplitForSelfCheck(true); });
            /* Drag and drop last, because it is the part of the surface a run with no hands cannot
             * do at all: the two panes are pointed at two folders of the watched tree, a file is
             * written into one of them, and a real drop event is posted at the other one's column -
             * once with no modifier, which is a move, and once with Ctrl, which is a copy. */
            QTimer::singleShot(8500, [&window]() { window.prepareTransferForSelfCheck(); });
            QTimer::singleShot(8900, [&window]() { window.moveAcrossPanesForSelfCheck(); });
            QTimer::singleShot(9500, [&window]() { window.copyBackAcrossPanesForSelfCheck(); });
            /* And the drop that is not a transfer: back into the folder the file is already in. A
             * person does this by accident, and the difference between "nothing to do" and "the core
             * refused it" is only visible in the sentence the window prints. */
            QTimer::singleShot(9800, [&window]() { window.dropInPlaceForSelfCheck(); });
            /* The undo, twice: the copy's duplicate goes, then the move goes back - real Ctrl+Z
             * keys, asserted on the disk. The stack at this point is [move, copy], which is why
             * this step sits after the drop that transferred nothing. */
            QTimer::singleShot(10100, [&window]() { window.undoForSelfCheck(); });
            /* The hover, between two real drag events - and before the probe goes away, because the
             * payload it carries is the probe. */
            QTimer::singleShot(10350, [&window, target]() {
                const QFileInfo shot(target);
                window.dropHoverForSelfCheck(shot.dir().filePath(
                    shot.completeBaseName() + QStringLiteral(".drophover.png")));
            });
            /* The same opt-in undo as the probe's, a beat after the copy has left a file at each end
             * of the tree, so the run finishes with nothing of its own in the watched folder. */
            QTimer::singleShot(10800, [&window]() { window.removeSelfCheckProbe(); });
        }
        /* The depth cap is only crossed by going deep, and crossing it is what drops a column from
         * the front of the chain - the one structural change that makes every captured position
         * wrong. So the gate walks down N folders and then clicks a header, which is the call that
         * dereferences a column by what it captured. */
        const int deep = parser.value(deepOption).toInt();
        for (int step = 0; step < deep; ++step) {
            QTimer::singleShot(2000 + step * 300, [&window]() { window.descendForSelfCheck(); });
        }
        if (deep > 0) {
            QTimer::singleShot(2000 + deep * 300,
                               [&window]() { window.clickHeaderForSelfCheck(); });
        }
        const int grabAt =
            std::max({2500, mutate ? 11300 : 0, deep > 0 ? 2800 + deep * 300 : 0});
        QTimer::singleShot(grabAt, [&window, &app, &boot, target]() {
            if (window.icons() != nullptr) {
                /* pending=0 is what makes the first two numbers a gate: the worker is idle, so
                 * extracted and destroyed can only differ by something that leaked. */
                qInfo("icons: extracted=%d destroyed=%d cached=%d pending=%d",
                      window.icons()->extracted(), window.icons()->destroyed(),
                      window.icons()->cached(), window.icons()->pending());
            }
            /* The round trips this shell asked for, over the whole run. A poll cycle should cost one
             * poll plus the headers the paint needed; a `retries` above zero means buffers came back
             * truncated and the shell paid twice for an answer it already had. */
            const FamIpcCount ipc = window.ipcCount();
            qInfo("ipc: headers=%d pages=%d polls=%d retries=%d tags=%d notes=%d total=%d",
                  ipc.headers, ipc.pages, ipc.polls, ipc.retries, ipc.tagReads, ipc.noteReads,
                  ipc.total());
            if (window.grab().save(target)) {
                qInfo("self-check wrote %s at %lldms",
                      qUtf8Printable(QDir::toNativeSeparators(target)), boot.elapsed());
            } else {
                qWarning("self-check could not write %s", qUtf8Printable(target));
            }
            app.quit();
        });
        // Never let a broken render hang a headless run.
        QTimer::singleShot(grabAt + 2000, [&app]() { app.quit(); });
    }

    if (parser.isSet(watchOption)) {
        qInfo("about to open %lldms after process entry", boot.elapsed());
        QString error;
        /* Remembered before the open, so the timeline can put this same folder in the other pane
         * later - a drive pick along the way must not move what "the watched folder" means. */
        window.noteWatchedFolder(parser.value(watchOption));
        if (!window.openFolder(parser.value(watchOption), &error)) {
            qWarning("could not open %s: %s", qUtf8Printable(parser.value(watchOption)),
                     qUtf8Printable(error));
        }
    }

    return app.exec();
}
