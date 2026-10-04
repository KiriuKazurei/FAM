#ifndef COLUMNCHAIN_H
#define COLUMNCHAIN_H

#include <QModelIndex>
#include <QString>
#include <QStringList>
#include <QVector>
#include <QWidget>

#include "bridge.h"
#include "fileview.h"

class FamListModel;
class QHBoxLayout;
class IconCache;

/* A Miller column chain: one directory per column, one v2 token per column, one model per column.
 *
 * The plan asked for `QColumnView` with "one QAbstractListModel per column, 1:1 with a dir_token",
 * and those two halves do not fit together: QColumnView drives its columns from a *single* model
 * through the parent/index tree protocol, and its columns are QListView widgets with no header - so
 * the per-column sort indicator the plan also asks for cannot exist on it. This keeps the intent
 * (columns, one token each, a clickable sort control per column) and reuses the windowed model the
 * rest of the shell already exercises, instead of rebuilding windowing inside a tree model.
 *
 * What it owns:
 *   - the tokens, and closing them: a column that goes away closes its view, which is what lets the
 *     core stop watching a directory nobody is looking at;
 *   - the poll. Every column is polled on every beat, including while search results are on screen,
 *     because a view nobody drains queues its cut positions for as long as it lives;
 *   - the sort. Each column's control calls `fam_dir_set_sort` for that column's token only -
 *     sorting a folder is a property of that folder's view, not of the chain.
 *   - the drop target. A column is a FileView that names its own folder, so a drag landing in column
 *     three goes into the directory column three is showing, whatever the rest of the chain or the
 *     other pane happens to be on. The chain only relays what the column asked for.
 */
class ColumnChain final : public QWidget {
    Q_OBJECT

public:
    ColumnChain(FamBridge *bridge, IconCache *icons, const QStringList &kinds,
                QWidget *parent = nullptr);

    /* Shows `path` as the deepest column, dropping anything deeper. False when the core refused the
     * path; the chain is left as it was. */
    bool open(const QString &path);
    /* Replaces the whole chain with `path` as its first column - what "Open folder…" means, as
     * opposed to clicking a folder inside a column, which appends. */
    bool openRoot(const QString &path);
    /* Drops the deepest column. Returns the path that is now deepest, or empty when nothing is left. */
    QString goBack();
    void clear();

    /* One beat for every column on screen. Returns a sentence about what changed, empty when nothing
     * did - the caller owns the status bar, the chain owns the row arithmetic. */
    QString poll();

    /* Tells every column's model that a tag answer moved, so the next beat re-reads it even though no
     * row changed. A tag edit through this process is exactly that case: the file is still in the same
     * place with the same size, and only the store knows anything happened. */
    void invalidateTags();

    QString deepestPath() const {
        return m_columns.isEmpty() ? QString() : m_columns.last()->path;
    }
    int depth() const { return m_columns.size(); }
    bool isEmpty() const { return m_columns.isEmpty(); }

    /* The head of the deepest column, for a caller that wants to say something true about the
     * directory on screen. */
    FamWindow deepestHead() const;

    /* The row the reader is on: the deepest column's selection, or that column's own directory when
     * nothing has been picked yet. This is what "tag selected" acts on. */
    QString selectedPath() const;

    /* The view a scroll, a keyboard shortcut or a headless drop should act on. */
    FileView *deepestView() const {
        return m_columns.isEmpty() ? nullptr : m_columns.last()->view;
    }

    /* The deepest column's model, for a caller that reads rows rather than paints them - the headless
     * gate's `TagsRole` claim, which is about what the beat put in the cache and cannot be made from a
     * screenshot. */
    FamListModel *deepestModel() const {
        return m_columns.isEmpty() ? nullptr : m_columns.last()->model;
    }

    /* Picks the first folder in the deepest column, which is what makes the next column appear. The
     * headless gate needs this because navigation is otherwise a mouse event, and a chain that has
     * never opened a second column is a chain nobody has tested. */
    bool selectFirstFolder();

    /* Cycles the deepest column's sort, as if its header had been clicked, and returns what it is
     * sorted by afterwards - empty when there is no column. The gate clicks it because a sort control
     * nobody pressed is the same unfinished wire it was before this increment. */
    QString sortDeepest();

    /* Clicks the deepest column's header button for real, so the connected lambda runs the way a
     * person's click does. Calling the sort directly would skip the one path that resolves a column
     * by the token the button captured. */
    bool clickDeepestHeader();

signals:
    /* A row was dropped onto one of the columns, and `folder` is that column's own directory. Emitted
     * by the column's view and relayed unchanged - the chain has no idea which pane it belongs to, and
     * the caller has to know that to say where the file came from. */
    void dropped(const QString &folder, const QStringList &paths, bool copy);
    /* A tag chip on one of the columns' rows was clicked; straight up to the pane, which relays
     * it with the results list's own. */
    void tagActivated(const QString &path, const QString &tag);
    /* The keyboard's file commands, relayed straight up with the row's path. */
    void deleteRequested(const QString &path);
    void renameRequested(const QString &path);
    void openRequested(const QString &path);
    /* The selected row changed: the preview should show this path. Emitted for directories and
     * files alike — the receiver decides what to show. */
    void previewChanged(const QString &path, bool isDirectory);
    /* The person asked for a new column (right-click on a directory row). The chain opens it. */
    void openInColumnRequested(const QString &path);

private:
    struct Column {
        quint64 token = 0;
        QString path;
        FamListModel *model = nullptr;
        /* A FileView rather than a QListView because being a drop target for `path` is this view's
         * job, and the folder it drops into is set once here and never again. */
        FileView *view = nullptr;
        QWidget *frame = nullptr;
        /* What this column is sorted by, so a click can cycle name -> size -> modified without asking
         * the core what it currently is: the core has no getter for a view's order. */
        QString sort = QStringLiteral("name");
        QString order = QStringLiteral("ascending");
        FamWindow head;
    };

    /* The selection handler one column's current row change runs. `previous` comes with the signal and
     * is logged, because the difference between "the reader moved down a row" and "a re-read left the
     * current index pointing at a different item" is only visible in where it came from. */
    void onCurrentChanged(quint64 token, const QModelIndex &previous);
    void onHeaderClicked(Column &column);
    void applySortTo(Column &column);
    Column *addColumn(const QString &path, quint64 token);
    /* Columns are found by token, never by position. A captured position goes stale the moment a
     * column is dropped from the front - and the depth cap does exactly that, which is how the
     * first version of this file managed to index one past the end of the vector. Tokens are
     * monotonic and never reused, so a token that is not in the chain means the column is gone. */
    Column *find(quint64 token);
    int indexOf(const Column *column) const;
    void dropColumn(Column *column);
    void dropDeepest();

    FamBridge *m_bridge;
    IconCache *m_icons;
    QStringList m_kinds;
    QHBoxLayout *m_layout;
    QVector<Column *> m_columns;
    /* Guards the selection handler against the recursion a programmatic column change causes:
     * opening a column selects its first row, which would otherwise open the next one forever. */
    bool m_filling = false;
};

#endif /* COLUMNCHAIN_H */
