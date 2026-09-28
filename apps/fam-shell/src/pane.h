#ifndef PANE_H
#define PANE_H

#include <QWidget>

#include "columnchain.h"
#include "famlistmodel.h"

class IconCache;
class QStackedWidget;

/* One browsing pane: a chain of Miller columns, plus the search results that can cover them.

 * The window holds one or two of these. Everything that used to be *the* chain, *the* results list
 * and *the* running search lives here, because with two panes those words stop referring to one
 * thing - and a second pane that shared a search token with the first would have its results
 * rewritten by the other one's poll, which is a data race dressed up as a layout.
 *
 * What a pane does *not* own: the bridge, the icon cache and the kind list. Those are per-process,
 * and the core's view tokens are per-view rather than per-window, so two panes opening the same
 * folder get two tokens and each follows its own watch.
 *
 * The two pages are a stack, not a swap of models, for the reason increment 8a-1 established: the
 * columns stay open and polled behind the results, so coming back from a search is a page flip
 * instead of a re-read of every folder in the chain. */
class Pane final : public QWidget {
    Q_OBJECT

public:
    Pane(FamBridge *bridge, IconCache *icons, const QStringList &kinds,
         QWidget *parent = nullptr);

    /* The chain is the folder side of the pane; the model and view below it are the search side.
     * Public because the window wires its toolbar, status line and keyboard to whichever pane has
     * the focus, and it needs all four to do that. */
    ColumnChain *chain = nullptr;
    FamListModel *model = nullptr;
    /* A FileView that was never given a folder to drop into: its rows come from wherever the query
     * reached, so it is a drag source and not a target. */
    FileView *results = nullptr;
    QStackedWidget *pages = nullptr;

    /* The search this pane started, if one is still arriving. */
    quint64 searchToken = 0;
    bool searchRunning = false;

    bool showingResults() const { return pages != nullptr && m_resultsPage == 1; }
    void showResults();
    void showFolder();

    /* Both halves of the pane: the columns and the results list keep their own tag caches, and a tag
     * edit from either one is invisible to the other. */
    void invalidateTags() {
        chain->invalidateTags();
        model->invalidateTags();
    }

signals:
    /* Emitted when the pointer lands anywhere inside this pane. The window keeps one "active" pane
     * because the search box, the scope selector and the Tag button each act on exactly one of them,
     * and a second guess about which one a person meant is worse than asking them to click it. */
    void activated();

    /* One of this pane's columns took a drop. Relayed straight from the chain, because the folder a
     * drop lands in is the column's own and the window only needs to know a transfer was asked for. */
    void dropped(const QString &folder, const QStringList &paths, bool copy);

protected:
    /* Child views and their viewports swallow the press before a parent widget would see it, so the
     * only way to learn "someone is working in this pane" is to watch them. */
    bool eventFilter(QObject *watched, QEvent *event) override;

private:
    void watch(QWidget *widget);
    int m_resultsPage = 0;
};

#endif /* PANE_H */
