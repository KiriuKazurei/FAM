/* End-to-end check of the C ABI without any GUI: start the core against an isolated config, browse
 * a directory, search it, then create and delete a file and confirm both come back through the
 * channels a shell actually uses.
 *
 * This used to drive v1's JSON view (`fam_view_new` / `fam_view_search` / `fam_view_drain`), because
 * the claim it gated was "a v1 consumer still gets answers out of this library". Increment 9 deleted
 * that surface, so the gate moved to v2 - and moving it is what makes it gate something worth
 * gating: the create half only passes because the watch now asks for the re-read itself. Against the
 * code before that fix, this program waits ten seconds and returns 1, because the new file never
 * arrives while the folder is open.
 *
 * Exits 0 only when all eight claims hold: the new row was seen, its removal came back as a cut, the
 * three transfer rules behaved through the real DLL, and the two tag reads answer in the shape the
 * shell joins on.
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

namespace {

constexpr int kTimeoutMs = 10000;
constexpr const char *kProbeName = "probe-created-by-smoke.txt";

void dump(const char *label, const QJsonValue &value) {
    const QByteArray text = value.isArray()
                                ? QJsonDocument(value.toArray()).toJson(QJsonDocument::Compact)
                                : QJsonDocument(value.toObject()).toJson(QJsonDocument::Compact);
    printf("%s %s\n", label, text.constData());
}

/* What is actually in a file, for the claims that are only true if the bytes did not move. A failed
 * read answers empty, which is enough: an empty comparison against a file that had content fails,
 * and that is the direction this gate wants to fail in. */
QByteArray readFileBytes(const QString &path) {
    QFile file(path);
    if (!file.open(QIODevice::ReadOnly)) {
        return QByteArray();
    }
    return file.readAll();
}

/* Polls until `ask` says it is done, the way the shell's 250ms timer does - just faster, because a
 * smoke gate should not spend a human's minute waiting for one file. */
template <typename Try> bool until(Try ask) {
    QElapsedTimer clock;
    clock.start();
    while (clock.elapsed() < kTimeoutMs) {
        if (ask()) {
            return true;
        }
        QThread::msleep(50);
    }
    return false;
}

int indexOfRow(const FamWindow &window, const QString &name) {
    for (int row = 0; row < window.rows.size(); ++row) {
        if (window.rows.at(row).name == name) {
            return row;
        }
    }
    return -1;
}

} /* namespace */

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

    QStringList kinds, backends, available;
    bridge.coreBackends(&kinds, &backends, &available, &error);
    printf("backends=%s available=%s\n", qUtf8Printable(backends.join(QLatin1Char(','))),
           qUtf8Printable(available.join(QLatin1Char(','))));

    /* --- browse: open without waiting, then let the loader's rows arrive --------------------- */

    quint64 dir = 0;
    if (!bridge.openDir(tree, &dir, &error)) {
        fprintf(stderr, "open_dir failed: %s\n", qUtf8Printable(error));
        return 1;
    }
    FamWindow head;
    if (!until([&]() {
            return bridge.dirStatus(dir, &head, &error) && head.state == FAM_READY
                && head.totalRows > 0;
        })) {
        fprintf(stderr, "directory never became ready: %s\n", qUtf8Printable(error));
        return 1;
    }
    printf("dir token=%llu rows=%u generation=%llu\n", static_cast<unsigned long long>(dir),
           head.totalRows, static_cast<unsigned long long>(head.generation));

    FamWindow page;
    if (!bridge.dirFetch(dir, 0, 10, &page, &error) || page.rows.isEmpty()) {
        fprintf(stderr, "first window empty: %s\n", qUtf8Printable(error));
        return 1;
    }
    printf("first row=%s kind=%u directory=%d\n", qUtf8Printable(page.rows.first().name),
           page.rows.first().kind, page.rows.first().directory ? 1 : 0);

    /* --- search: begin without waiting, page what arrives while it runs ---------------------- */

    quint64 search = 0;
    /* "file" because the tree this runs against holds file-1..3.txt. The old gate searched for
     * "probe" before creating the probe file and so always printed zero rows - a number that proves
     * nothing either way. */
    if (!bridge.beginSearch(QStringLiteral("file"), QString(), QString(), 0, &search, &error)) {
        fprintf(stderr, "search_begin failed: %s\n", qUtf8Printable(error));
        return 1;
    }
    FamWindow settled;
    until([&]() {
        return bridge.searchStatus(search, &settled, &error) && settled.state != FAM_LOADING;
    });
    FamWindow results;
    bridge.searchPage(search, 0, 20, &results, &error);
    printf("search state=%u rows=%d incomplete=%d\n", settled.state, results.rows.size(),
           results.partial() ? 1 : 0);
    bridge.closeSearch(search);

    /* --- the create: the watch must bring the new row in without anyone reopening the folder -- */

    const QString probe = QDir(tree).filePath(QLatin1String(kProbeName));
    {
        QFile file(probe);
        if (!file.open(QIODevice::WriteOnly)) {
            fprintf(stderr, "cannot create probe file: %s\n", qUtf8Printable(file.errorString()));
            return 1;
        }
        file.write("x");
    }

    int insertedAt = -1;
    const bool sawInsert = until([&]() {
        QVector<quint32> cuts;
        FamWindow polled;
        if (!bridge.dirCuts(dir, &polled, &cuts, &error)) {
            return false;
        }
        if (polled.resync()) {
            /* Rows were replaced by a re-read - which is exactly what a create has to cause, since
             * the event carries no size, no time and no answer to "directory?". */
            FamWindow fresh;
            if (!bridge.dirFetch(dir, 0, 50, &fresh, &error)) {
                return false;
            }
            insertedAt = indexOfRow(fresh, QLatin1String(kProbeName));
            return insertedAt >= 0;
        }
        return false;
    });
    if (sawInsert) {
        printf("observed row at index %d\n", insertedAt);
    } else {
        fprintf(stderr, "the new file never reached the open view\n");
    }

    /* --- the delete: removals are placeable from the event alone, so they come back as a cut -- */

    /* Removed once, before the polling - not inside it. A second `QFile::remove` of a file that is
     * already gone returns false, which in a poll loop reads as "not yet" and then never asks the
     * core again until the timeout. */
    QFile::remove(probe);
    int cutPositions = 0;
    const bool sawCut = sawInsert && until([&]() {
                            QVector<quint32> cuts;
                            FamWindow polled;
                            if (!bridge.dirCuts(dir, &polled, &cuts, &error)) {
                                return false;
                            }
                            cutPositions = cuts.size();
                            return !cuts.isEmpty();
                        });
    if (sawCut) {
        printf("observed %d cut position(s)\n", cutPositions);
    } else if (sawInsert) {
        fprintf(stderr, "the deletion never came back as a cut\n");
    }

    /* --- the transfer: the first call on this surface that changes the disk -------------------
     *
     * Worth running through the real DLL rather than trusting the Rust unit tests, because the part
     * under test here is the boundary: two C strings in, one JSON object out, and a refusal that
     * arrives as a message instead of a half-done move. The rules themselves are tested where they
     * live. */

    const QString moved = tree + QStringLiteral("/fam-abi-moved.txt");
    const QString kept = tree + QStringLiteral("/fam-abi-kept.txt");
    const QString twin = tree + QStringLiteral("/fam-abi-twin.txt");
    QFile::remove(moved);
    QFile::remove(kept);
    QFile::remove(twin);
    auto seed = [](const QString &path) {
        QFile file(path);
        if (!file.open(QIODevice::WriteOnly)) {
            return false;
        }
        file.write("content\n");
        file.close();
        return true;
    };

    bool movedOk = false, copiedOk = false, overwriteRefused = false;
    if (seed(tree + QStringLiteral("/fam-abi-move.txt")) && seed(kept)) {
        QJsonValue answer;
        if (!bridge.pathOp(tree + QStringLiteral("/fam-abi-move.txt"), moved, false, &answer,
                           &error)) {
            fprintf(stderr, "the move failed: %s\n", qUtf8Printable(error));
        } else {
            /* The source not being there any more is the whole claim; `by` says which way the bytes
             * got moved, and on one volume that had better be a rename. */
            movedOk = !QFile::exists(tree + QStringLiteral("/fam-abi-move.txt"))
                && QFile::exists(moved)
                && answer.toObject().value(QStringLiteral("by")).toString()
                    == QStringLiteral("renamed");
        }

        if (!bridge.pathOp(kept, twin, true, &answer, &error)) {
            fprintf(stderr, "the copy failed: %s\n", qUtf8Printable(error));
        } else {
            copiedOk = QFile::exists(kept) && QFile::exists(twin)
                && answer.toObject().value(QStringLiteral("by")).toString()
                    == QStringLiteral("copied");
        }

        /* The drag that would have replaced a file: refused, and both files exactly as they were. */
        const QString onto = twin;
        const QByteArray before = readFileBytes(onto);
        const bool accepted = bridge.pathOp(moved, onto, false, &answer, &error);
        overwriteRefused = !accepted && readFileBytes(onto) == before && QFile::exists(moved);
        if (accepted) {
            fprintf(stderr, "an overwrite was allowed: %s onto %s\n", qUtf8Printable(moved),
                    qUtf8Printable(onto));
        } else {
            printf("overwrite refused: %s\n", qUtf8Printable(error));
        }
    }

    /* --- the two reads a listing needs in order to show tags ------------------------------------
     *
     * The claim worth crossing the boundary for is the one a filter bar makes out loud: its number
     * counts **files**, not rows. One name sitting on one file under both sources is two rows and one
     * file, so a bar that said "3" over two files would be promising a result set it cannot deliver.
     * The rest pins the shape of the batch answer, because the shell joins on it. */

    bool vocabularyCountsFiles = false;
    bool batchKeysByCaller = false;
    bool emptyWindowIsAnAnswer = false;
    {
        const QString firstTagged = tree + QStringLiteral("/fam-abi-tag-a.txt");
        const QString secondTagged = tree + QStringLiteral("/fam-abi-tag-b.txt");
        const QString untagged = tree + QStringLiteral("/fam-abi-tag-c.txt");
        const bool seeded = seed(firstTagged) && seed(secondTagged) && seed(untagged);
        if (!seeded) {
            fprintf(stderr, "the tag reads could not be run: no files to tag\n");
        }
        static const char *kTag = "fam-abi-tag";
        QJsonValue answer;
        bridge.tagAdd(firstTagged, {QString::fromLatin1(kTag)}, QStringLiteral("manual"), 0, &answer,
                      &error);
        /* The same name from the other source, on the same file: this is the row that must not make
         * the count go to three. */
        bridge.tagAdd(firstTagged, {QString::fromLatin1(kTag)}, QStringLiteral("auto"), 1, &answer,
                      &error);
        bridge.tagAdd(secondTagged, {QString::fromLatin1(kTag)}, QStringLiteral("manual"), 0, &answer,
                      &error);

        QVector<TagFacet> facets;
        if (seeded && bridge.tagVocabulary(&facets, &error)) {
            for (const TagFacet &facet : facets) {
                if (facet.tag == QLatin1String(kTag)) {
                    vocabularyCountsFiles = facet.files == 2;
                    printf("vocabulary: %s on %d file(s)\n", qUtf8Printable(facet.tag), facet.files);
                }
            }
        } else if (seeded) {
            fprintf(stderr, "the vocabulary read failed: %s\n", qUtf8Printable(error));
        }

        QHash<QString, QStringList> tags;
        if (seeded
            && bridge.tagsForPaths(QStringList{firstTagged, secondTagged, untagged}, &tags, &error)) {
            /* Two claims in one: the file with no tag is **absent** rather than present with an empty
             * list, and the keys are the strings this caller sent - not the library's folded form of a
             * path, which is a caller with no business reproducing it. */
            batchKeysByCaller = tags.size() == 2 && !tags.contains(untagged)
                && tags.value(firstTagged) == QStringList{QString::fromLatin1(kTag)}
                && tags.value(secondTagged) == QStringList{QString::fromLatin1(kTag)};
        } else if (seeded) {
            fprintf(stderr, "the batch tag read failed: %s\n", qUtf8Printable(error));
        }

        /* An empty window is a normal question, from a shell that opened an empty folder. */
        emptyWindowIsAnAnswer = bridge.tagsForPaths(QStringList{}, &tags, &error) && tags.isEmpty();

        /* One call per file with no source: removing by name takes both rows, which is the same
         * semantics the shell's tag dialog had to draw around. */
        bridge.tagRemove(firstTagged, {QString::fromLatin1(kTag)}, &answer, &error);
        bridge.tagRemove(secondTagged, {QString::fromLatin1(kTag)}, &answer, &error);
        QFile::remove(firstTagged);
        QFile::remove(secondTagged);
        QFile::remove(untagged);
    }
    printf("vocabulary-files-not-rows=%d batch-keyed-by-caller=%d empty-window-answers=%d\n",
           vocabularyCountsFiles ? 1 : 0, batchKeysByCaller ? 1 : 0, emptyWindowIsAnAnswer ? 1 : 0);

    QFile::remove(moved);
    QFile::remove(kept);
    QFile::remove(twin);
    QFile::remove(tree + QStringLiteral("/fam-abi-move.txt"));
    printf("move=%d copy=%d overwrite-refused=%d\n", movedOk ? 1 : 0, copiedOk ? 1 : 0,
           overwriteRefused ? 1 : 0);

    bridge.closeDir(dir);
    const bool passed = sawInsert && sawCut && movedOk && copiedOk && overwriteRefused
        && vocabularyCountsFiles && batchKeysByCaller && emptyWindowIsAnAnswer;
    printf("insert=%d cut=%d move=%d copy=%d refuse-overwrite=%d vocabulary=%d batch=%d empty=%d"
           " -> %s\n",
           sawInsert ? 1 : 0, sawCut ? 1 : 0, movedOk ? 1 : 0, copiedOk ? 1 : 0,
           overwriteRefused ? 1 : 0, vocabularyCountsFiles ? 1 : 0, batchKeysByCaller ? 1 : 0,
           emptyWindowIsAnAnswer ? 1 : 0, passed ? "ok" : "FAILED");
    return passed ? 0 : 1;
}
