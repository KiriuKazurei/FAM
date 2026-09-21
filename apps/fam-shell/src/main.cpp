#include <QApplication>
#include <QCommandLineParser>
#include <QFile>
#include <QTextStream>
#include <QDir>
#include <QPixmap>
#include <QTimer>

#include "famcore.h"
#include "mainwindow.h"

int main(int argc, char *argv[]) {
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
    parser.addOption(configOption);
    parser.addOption(queryOption);
    parser.addOption(watchOption);
    parser.addOption(grabOption);
    parser.process(app);

    MainWindow window(parser.value(configOption), parser.value(queryOption));
    window.setWindowTitle(QStringLiteral("FAM — resource manager"));
    window.resize(960, 620);
    window.show();

    if (parser.isSet(grabOption)) {
        const QString target = parser.value(grabOption);
        /* A windowed executable has no console, so the headless gate would otherwise prove nothing
         * but pixels: what the view did between showing and grabbing goes to a log beside the
         * screenshot. */
        static QFile log(target + QStringLiteral(".log"));
        static QTextStream out(&log);
        if (log.open(QIODevice::WriteOnly | QIODevice::Truncate)) {
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
        QTimer::singleShot(2500, [&window, &app, target]() {
            if (window.grab().save(target)) {
                qInfo("self-check wrote %s", qUtf8Printable(QDir::toNativeSeparators(target)));
            } else {
                qWarning("self-check could not write %s", qUtf8Printable(target));
            }
            app.quit();
        });
        // Never let a broken render hang a headless run.
        QTimer::singleShot(8000, [&app]() { app.quit(); });
    }

    if (parser.isSet(watchOption)) {
        QString error;
        if (!window.openFolder(parser.value(watchOption), &error)) {
            qWarning("could not open %s: %s", qUtf8Printable(parser.value(watchOption)),
                     qUtf8Printable(error));
        }
    }

    return app.exec();
}
