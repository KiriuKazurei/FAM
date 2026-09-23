#include <QApplication>
#include <QCommandLineParser>
#include <QElapsedTimer>
#include <QFile>
#include <QTextStream>
#include <QDir>
#include <QPixmap>
#include <QTimer>

#include "famcore.h"
#include "iconcache.h"
#include "mainwindow.h"

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
        QStringLiteral("With --watch and --self-check: create one file in the watched folder and "
                       "remove it again, so a re-read happens while a view is open."));
    parser.addOption(configOption);
    parser.addOption(queryOption);
    parser.addOption(watchOption);
    parser.addOption(grabOption);
    parser.addOption(mutateOption);
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
            QTimer::singleShot(5500, [&window]() { window.removeSelfCheckProbe(); });
            /* Last, because it replaces the chain: a drive pick is the sidebar doing its only job,
             * and the frame after it shows what the reader would see. */
            QTimer::singleShot(6100, [&window]() { window.pickFirstDriveForSelfCheck(); });
        }
        const int grabAt = mutate ? 7200 : 2500;
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
            qInfo("ipc: headers=%d pages=%d polls=%d retries=%d total=%d", ipc.headers, ipc.pages,
                  ipc.polls, ipc.retries, ipc.total());
            if (window.grab().save(target)) {
                qInfo("self-check wrote %s at %lldms",
                      qUtf8Printable(QDir::toNativeSeparators(target)), boot.elapsed());
            } else {
                qWarning("self-check could not write %s", qUtf8Printable(target));
            }
            app.quit();
        });
        // Never let a broken render hang a headless run.
        QTimer::singleShot(mutate ? 13000 : 8000, [&app]() { app.quit(); });
    }

    if (parser.isSet(watchOption)) {
        qInfo("about to open %lldms after process entry", boot.elapsed());
        QString error;
        if (!window.openFolder(parser.value(watchOption), &error)) {
            qWarning("could not open %s: %s", qUtf8Printable(parser.value(watchOption)),
                     qUtf8Printable(error));
        }
    }

    return app.exec();
}
