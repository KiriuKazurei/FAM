#include <QApplication>
#include <QCommandLineParser>
#include <QDir>
#include <QPixmap>
#include <QTimer>

#include "famcore.h"
#include "mainwindow.h"

int main(int argc, char *argv[]) {
    if (fam_abi_version() != 1) {
        qWarning("famcore.dll exposes ABI version %u but this shell was built for 1",
                 fam_abi_version());
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
        QStringLiteral("watch"), QStringLiteral("Start watching <path> once shown."),
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

    if (parser.isSet(watchOption)) {
        QString error;
        if (!window.startWatching(parser.value(watchOption), &error)) {
            qWarning("watch failed: %s", qUtf8Printable(error));
        }
    }

    if (parser.isSet(grabOption)) {
        const QString target = parser.value(grabOption);
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

    return app.exec();
}
