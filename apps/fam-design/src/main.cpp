#include <QApplication>
#include <QCommandLineParser>
#include <QDir>
#include <QPixmap>
#include <QStandardPaths>
#include <QTextStream>
#include <QTimer>

#include "preview.h"
#include "theme.h"

/* The design sample's own entry point.
 *
 * `--shot <path>` renders, grabs the window and quits, which is how both schemes get looked at
 * without anyone having to click through them; `--theme` picks the scheme, defaulting to the
 * desktop's. The measured contrast table goes to stdout in either mode, because the number is the
 * part of this program worth reading even when nobody is going to look at the pixels. */
int main(int argc, char *argv[]) {
    QApplication app(argc, argv);
    QApplication::setApplicationName(QStringLiteral("FAM design sample"));
    QApplication::setApplicationVersion(QStringLiteral("0.1.0"));

    QCommandLineParser parser;
    parser.setApplicationDescription(
        QStringLiteral("Ant Design 5 treatment of the FAM shell - a sample, not the product"));
    parser.addHelpOption();
    parser.addVersionOption();
    const QCommandLineOption themeOption(
        QStringLiteral("theme"),
        QStringLiteral("light, dark or system (the default)."), QStringLiteral("scheme"));
    const QCommandLineOption dirOption(QStringLiteral("dir"),
                                       QStringLiteral("Show this folder instead of the documents "
                                                      "directory."),
                                       QStringLiteral("path"));
    const QCommandLineOption shotOption(QStringLiteral("shot"),
                                        QStringLiteral("Render, save a screenshot to <path>, quit."),
                                        QStringLiteral("path"));
    parser.addOption(themeOption);
    parser.addOption(dirOption);
    parser.addOption(shotOption);
    parser.process(app);

    /* Fusion, not the native Windows style: the look here is entirely the style sheet's, and a style
     * that draws its own gradients and focus glows wherever the sheet is silent is a style that will
     * disagree with the token set. */
    QApplication::setStyle(QStringLiteral("Fusion"));
    Scheme scheme = Theme::systemScheme();
    const QString requested = parser.value(themeOption).toLower();
    if (requested == QLatin1String("light")) {
        scheme = Scheme::Light;
    } else if (requested == QLatin1String("dark")) {
        scheme = Scheme::Dark;
    } else if (!requested.isEmpty() && requested != QLatin1String("system")) {
        qWarning("unknown --theme \"%s\"; using %s", qUtf8Printable(requested),
                 scheme == Scheme::Dark ? "dark" : "light");
    }
    Theme::instance().setScheme(scheme);
    app.setFont(Theme::instance().applicationFont());
    app.setStyleSheet(Theme::instance().styleSheet());

    QTextStream console(stdout);
    console << "scheme: " << Theme::instance().name() << '\n'
            << "contrast:\n"
            << Theme::instance().contrastReport();
    console.flush();

    QString start = parser.value(dirOption);
    if (start.isEmpty()) {
        start = QStandardPaths::writableLocation(QStandardPaths::DocumentsLocation);
        if (start.isEmpty() || !QDir(start).exists()) {
            start = QDir::homePath();
        }
    }

    PreviewWindow window(start);
    window.setWindowTitle(QStringLiteral("FAM — Ant Design 5 样式样机"));
    window.resize(980, 640);
    window.show();

    if (parser.isSet(shotOption)) {
        const QString target = parser.value(shotOption);
        /* Long enough for the first folder read and the styles to settle; the grab is after the
         * event loop has run a few times, not inside the constructor. */
        QTimer::singleShot(400, [&window, &app, target]() {
            if (!window.grab().save(target)) {
                qWarning("could not write %s", qUtf8Printable(target));
            }
            app.quit();
        });
    }

    return app.exec();
}
