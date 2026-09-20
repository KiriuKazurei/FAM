/* End-to-end check of the C ABI without any GUI: start the core against an isolated config,
 * search, watch a directory, create a file and confirm the insert delta comes back.
 *
 * Exits 0 only when the delta was observed, so CI can use it as a smoke gate.
 */
#include <QCoreApplication>
#include <QDir>
#include <QElapsedTimer>
#include <QFile>
#include <QJsonArray>
#include <QJsonDocument>
#include <QJsonObject>
#include <QThread>

#include "bridge.h"
#include "famcore.h"

static void dump(const char *label, const QJsonValue &value) {
    const QByteArray text = value.isArray()
                                ? QJsonDocument(value.toArray()).toJson(QJsonDocument::Compact)
                                : QJsonDocument(value.toObject()).toJson(QJsonDocument::Compact);
    printf("%s %s\n", label, text.constData());
}

int main(int argc, char *argv[]) {
    QCoreApplication app(argc, argv);
    const QStringList arguments = app.arguments();
    if (arguments.size() < 3) {
        fprintf(stderr, "usage: abi-smoke <config.toml> <tree-dir>\n");
        return 2;
    }
    const QString config = arguments.at(1);
    const QString tree = arguments.at(2);

    printf("abi=%u\n", fam_abi_version());

    QString error;
    FamBridge bridge(config, &error);
    if (!bridge.healthy()) {
        fprintf(stderr, "core failed to start: %s\n", qUtf8Printable(error));
        return 1;
    }

    QJsonValue stats;
    if (!bridge.stats(&stats, &error)) {
        fprintf(stderr, "stats failed: %s\n", qUtf8Printable(error));
        return 1;
    }
    dump("stats:", stats);

    quint64 view = 0;
    if (!bridge.createView(QStringLiteral("name"), QStringLiteral("ascending"), &view, &error)) {
        fprintf(stderr, "view_new failed: %s\n", qUtf8Printable(error));
        return 1;
    }
    printf("view token=%llu\n", static_cast<unsigned long long>(view));

    QJsonValue searched;
    if (!bridge.search(view, QStringLiteral("probe"), QString(), QString(), &searched, &error)) {
        fprintf(stderr, "search failed: %s\n", qUtf8Printable(error));
        return 1;
    }
    const QJsonObject searchResult = searched.toObject();
    printf("search backend=%s rows=%d incomplete=%d\n",
           searchResult.value("backend").toString().toUtf8().constData(),
           searchResult.value("rows").toArray().size(),
           searchResult.value("incomplete").toBool() ? 1 : 0);

    quint64 watch = 0;
    if (!bridge.startWatch(tree, &watch, &error)) {
        fprintf(stderr, "watch_start failed: %s\n", qUtf8Printable(error));
        return 1;
    }
    printf("watch token=%llu\n", static_cast<unsigned long long>(watch));

    const QString probe = QDir(tree).filePath(QStringLiteral("probe-created-by-smoke.txt"));
    {
        QFile file(probe);
        if (!file.open(QIODevice::WriteOnly)) {
            fprintf(stderr, "cannot create probe file: %s\n", qUtf8Printable(file.errorString()));
            return 1;
        }
        file.write("x");
    }

    int inserts = 0;
    QElapsedTimer clock;
    clock.start();
    while (clock.elapsed() < 8000) {
        QJsonValue drained;
        if (bridge.drain(view, &drained, &error)) {
            const QJsonArray deltas = drained.toObject().value("deltas").toArray();
            if (!deltas.isEmpty()) {
                dump("drain:", drained);
            }
            for (const QJsonValue &value : deltas) {
                const QJsonObject delta = value.toObject();
                const QJsonObject row = delta.value("row").toObject();
                if (delta.value("type").toString() == QLatin1String("insert")
                    && row.value("name").toString() == QLatin1String("probe-created-by-smoke.txt")) {
                    ++inserts;
                    printf("observed insert at index %d kind=%s size=%lld\n",
                           delta.value("index").toInt(),
                           row.value("kind").toString().toUtf8().constData(),
                           static_cast<long long>(row.value("size").toDouble()));
                }
            }
        }
        if (inserts > 0) {
            break;
        }
        QThread::msleep(100);
    }

    QFile::remove(probe);
    printf("inserts observed=%d\n", inserts);
    return inserts > 0 ? 0 : 1;
}
