/* The model's diff, watched from the outside: what a view is *told* about a re-read, and what that
 * does to the row the reader is standing on.
 *
 * `reconcile` exists so that a folder changing does not cost a person their place in it, and the
 * mistake it can make is invisible in the row data: a directory whose size moved is the same row of the
 * listing saying something new, and a scan that only asks "is this row identical" calls that a row
 * removed and a row inserted. A view believes the removal, moves the current index down to the next row,
 * and the column chain reads that move as a click - which is how a folder that had merely been written
 * into came to open a subfolder nobody had touched. The headless run caught it in its log
 * (`current row 1 (was 0, 4 listed)`); this catches it in the signal, where the cause can be named.
 *
 * Exits 0 only when every case below announces what it should and leaves the reader's row alone.
 */
#include <QAbstractItemModel>
#include <QApplication>
#include <QFileInfo>
#include <QItemSelectionModel>
#include <QListView>
#include <QString>
#include <QVector>

#include "famlistmodel.h"

namespace {

/* The table behind the fetcher: a case changes the folder by changing this. */
QVector<FamViewRow> g_table;

FamViewRow makeRow(const QString &name, quint64 size, bool directory = false) {
    FamViewRow row;
    row.name = name;
    row.size = size;
    row.directory = directory;
    row.kind = directory ? 1 : 2;
    row.attrs = directory ? 0x10 : 0x80;
    return row;
}

bool fetch(quint32 first, quint32 count, FamWindow *window) {
    window->state = FAM_READY;
    window->flags = 0;
    window->totalRows = quint32(g_table.size());
    window->rows.clear();
    for (quint32 at = first; at < quint32(g_table.size()) && at < first + count; ++at) {
        window->rows.append(g_table.at(int(at)));
    }
    return true;
}

FamWindow head() {
    FamWindow window;
    window.state = FAM_READY;
    window.totalRows = quint32(g_table.size());
    return window;
}

/* What the model announced during one re-read. Connected after the window is materialised, so the
 * resets that setting up causes are not mistaken for the change under test. */
struct Announced {
    int removals = 0;
    int inserts = 0;
    int changes = 0;
    int removedFrom = -1;
    int removedTo = -1;
    int changedFrom = -1;
    int changedTo = -1;
};

void attach(QAbstractItemModel *model, Announced *seen) {
    QObject::connect(model, &QAbstractItemModel::rowsAboutToBeRemoved, model,
                     [seen](const QModelIndex &, int first, int last) {
                         ++seen->removals;
                         seen->removedFrom = first;
                         seen->removedTo = last;
                     });
    QObject::connect(model, &QAbstractItemModel::rowsAboutToBeInserted, model,
                     [seen](const QModelIndex &, int, int) { ++seen->inserts; });
    QObject::connect(model, &QAbstractItemModel::dataChanged, model,
                     [seen](const QModelIndex &topLeft, const QModelIndex &bottomRight) {
                         ++seen->changes;
                         seen->changedFrom = topLeft.row();
                         seen->changedTo = bottomRight.row();
                     });
}

int failures = 0;

void check(const char *what, bool ok, const QString &detail) {
    printf("  %-42s %s  %s\n", what, ok ? "ok" : "FAILED", qUtf8Printable(detail));
    if (!ok) {
        ++failures;
    }
}

/* One folder, one re-read, everything counted. The reader starts on row `startRow`, and the claim is
 * that they finish on the *same item* - which may be a different row number, because a row inserted
 * above genuinely does move the listing down under them. What must not happen is the current index
 * landing on something else, because the column chain reads that as a click and opens it. */
void run(const char *what, const QVector<FamViewRow> &before, const QVector<FamViewRow> &after,
         int startRow, int expectRemovals, int expectInserts, int expectChanges, int expectModified,
         int expectRows) {
    g_table = before;
    FamListModel model;
    QListView view;
    view.setModel(&model);
    model.bind(1, QStringLiteral("C:/smoke"), {QStringLiteral("file"), QStringLiteral("dir")},
               &fetch);
    model.readHead(head());
    /* Painting is what fills a window in the shell; `pathAt` is the same call from the same place, and
     * it is what gives `reconcile` a window to diff against. */
    model.pathAt(0);
    view.selectionModel()->setCurrentIndex(model.index(startRow, 0),
                                           QItemSelectionModel::ClearAndSelect |
                                               QItemSelectionModel::Rows);
    const QString started = model.pathAt(startRow);
    Announced seen;
    attach(&model, &seen);

    g_table = after;
    const auto outcome = model.reconcile(head());
    const int row = view.selectionModel()->currentIndex().row();
    const QString stillOn = model.pathAt(row);

    const int pairs = expectRemovals + expectInserts;
    /* `changes` counts signals and `modified` counts the rows they cover: two neighbours that both
     * grew are one signal over a range of two. */
    const bool ok = seen.removals == expectRemovals && seen.inserts == expectInserts &&
                    seen.changes == expectChanges && outcome.pairs == pairs &&
                    outcome.modified == expectModified && int(model.totalRows()) == expectRows &&
                    row >= 0 && !started.isEmpty() && started == stillOn;
    check(what, ok,
          QStringLiteral("pairs=%1/%2 removals=%3[%4,%5] inserts=%6 changes=%7[%8,%9] modified=%10 "
                         "rows=%11 current=%12 on %13")
              .arg(outcome.pairs)
              .arg(pairs)
              .arg(seen.removals)
              .arg(seen.removedFrom)
              .arg(seen.removedTo)
              .arg(seen.inserts)
              .arg(seen.changes)
              .arg(seen.changedFrom)
              .arg(seen.changedTo)
              .arg(outcome.modified)
              .arg(model.totalRows())
              .arg(row)
              .arg(stillOn.isEmpty() ? QStringLiteral("<nothing>")
                                     : QFileInfo(stillOn).fileName()));
}

const QVector<FamViewRow> kPlain{makeRow(QStringLiteral("2024-contract"), 0, true),
                                 makeRow(QStringLiteral("photos"), 0, true),
                                 makeRow(QStringLiteral("README.txt"), 7),
                                 makeRow(QStringLiteral("inbox"), 0, true)};

} /* namespace */

int main(int argc, char *argv[]) {
    QApplication app(argc, argv);

    /* A file arrives inside the first folder: that folder's own row grows, and a new row joins the
     * listing. The growth is a modification and the arrival is one insertion - nothing is removed, so
     * the reader on row 0 stays on row 0. Before the scan was aligned on identity this cost a removal
     * at row 0, and the view moved the reader down to the folder that came next. */
    QVector<FamViewRow> grew = kPlain;
    grew[0] = makeRow(QStringLiteral("2024-contract"), 4096, true);
    QVector<FamViewRow> withNew = grew;
    withNew.insert(1, makeRow(QStringLiteral("draft.txt"), 12));
    run("grown folder + new file: no removal", kPlain, withNew, 0, 0, 1, 1, 1, 5);

    /* The same growth with nothing else: one signal and no pairs at all. This is the case that used to
     * cost two pairs for a row that never moved. */
    run("grown folder alone: no pairs", kPlain, grew, 0, 0, 0, 1, 1, 4);

    /* A file really does leave, and the removal still has to be announced - the fix must not have
     * swallowed the channel that a deletion travels on. */
    const QVector<FamViewRow> oneLess{kPlain[0], kPlain[2], kPlain[3]};
    run("deleted file: one removal", kPlain, oneLess, 0, 1, 0, 0, 0, 3);

    /* A rename is a different item in the same place: identity says no match, so it costs the pair it
     * always cost. */
    QVector<FamViewRow> renamed = kPlain;
    renamed[2] = makeRow(QStringLiteral("NOTES.txt"), 7);
    run("renamed file: removal + insertion", kPlain, renamed, 0, 1, 1, 0, 0, 4);

    /* Two neighbours both change size: one `dataChanged` over the run they cover, not two signals and
     * not four pairs. */
    QVector<FamViewRow> twoGrown = kPlain;
    twoGrown[1] = makeRow(QStringLiteral("photos"), 512, true);
    twoGrown[2] = makeRow(QStringLiteral("README.txt"), 8);
    run("two rows grew side by side: one range", kPlain, twoGrown, 0, 0, 0, 1, 2, 4);

    /* And the same claim with the reader on the row that changed rather than on the first one, and a
     * new file joining above them: the row number moves and the item does not, which is the whole
     * difference between a listing that grew and a listing that stole the selection. */
    QVector<FamViewRow> aboveAndGrew = kPlain;
    aboveAndGrew[1] = makeRow(QStringLiteral("photos"), 512, true);
    aboveAndGrew.insert(0, makeRow(QStringLiteral("a-new.txt"), 3));
    run("reader on the changed row, new row above", kPlain, aboveAndGrew, 1, 0, 1, 1, 1, 5);

    printf("%s\n", failures == 0 ? "model-smoke: ok" : "model-smoke: FAILED");
    return failures == 0 ? 0 : 1;
}
